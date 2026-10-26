//! 系统指标的唯一来源。菜单栏、主窗口都只读这里产出的 `Snapshot`。

use crate::model::*;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{Networks, System};

/// 有窗口可见时的采样间隔。规格 O-03 的推荐值，用户尚未确认。
pub const INTERVAL_VISIBLE: Duration = Duration::from_secs(2);
/// 没有窗口可见时的采样间隔。
pub const INTERVAL_HIDDEN: Duration = Duration::from_secs(5);
pub const TOP_N: usize = 15;
/// 随快照下发的 CPU 和网速历史点数（菜单栏面板的柱图和折线）
pub const HIST_POINTS: usize = 30;

const BATTERY_TTL: Duration = Duration::from_secs(30);
const GPU_TTL: Duration = Duration::from_secs(4);
/// 温度和风扇变化慢，读一次温度要问几十个传感器
const THERMAL_TTL: Duration = Duration::from_secs(5);
/// 电池健康度几乎不变，而查一次要一百多毫秒
const HEALTH_TTL: Duration = Duration::from_secs(600);
const IFACE_TTL: Duration = Duration::from_secs(30);
/// 可清除空间读一次要 7 毫秒左右（空闲块不到 0.1 毫秒），而且变得慢；空闲块每次都读，清理后的变化马上能看到。
/// ponytail: 系统刚把可清除空间腾成空闲时，最多一分钟内会重复算一次，要更准就把 TTL 调小。
const PURGEABLE_TTL: Duration = Duration::from_secs(60);

/// 进程合并前的原始记录。
#[derive(Clone, Debug)]
pub struct RawProc {
    pub pid: u32,
    pub parent: Option<u32>,
    /// 系统认定的「责任进程」，见 `responsible_pid`
    pub responsible: Option<u32>,
    pub name: String,
    pub exe: Option<PathBuf>,
    pub mem_bytes: u64,
    pub cpu_pct: f32,
}

/// 路径里最外层的 `.app` 包名，如 `/Applications/Google Chrome.app/...Helper.app/...` → `Google Chrome`。
fn app_of(exe: &Path) -> Option<String> {
    exe.components()
        .find_map(|c| c.as_os_str().to_str()?.strip_suffix(".app").map(str::to_owned))
}

/// 合并规则：沿父进程一路往上，归到最顶层的那个 `.app`。所以应用启动的另一个应用也算在它头上
/// （Claude 桌面端启动的 Claude Code 住在单独的 `claude.app` 里，用户要求两者合成一行）。
/// 自己不在任何 `.app` 里的，先从它的责任进程往上找
/// （Docker 的虚拟机是系统代为启动的，父进程不是 Docker，只有责任进程指向它），
/// 再从自己往上找；都没有就单独列出。结果按内存降序。
pub fn group_processes(raw: &[RawProc]) -> Vec<Proc> {
    let by_pid: HashMap<u32, &RawProc> = raw.iter().map(|p| (p.pid, p)).collect();
    // 从某个进程起沿父进程一路往上，取最顶层的那个 `.app`
    let app_up = |start: Option<&RawProc>| -> Option<String> {
        let (mut cur, mut top) = (start, None);
        // 上限防止父子关系成环
        for _ in 0..64 {
            let Some(c) = cur else { break };
            top = c.exe.as_deref().and_then(app_of).or(top);
            cur = c.parent.and_then(|pp| by_pid.get(&pp).copied());
        }
        top
    };
    let owner = |p: &RawProc| -> Option<String> {
        if p.exe.as_deref().and_then(app_of).is_some() {
            return app_up(Some(p));
        }
        let responsible = p.responsible.filter(|r| *r != p.pid).and_then(|r| by_pid.get(&r).copied());
        app_up(responsible).or_else(|| app_up(Some(p)))
    };

    let mut groups: HashMap<String, Proc> = HashMap::new();
    for p in raw {
        // 不属于任何应用的进程用「名称#进程号」做键，保证互不合并
        let (key, name) = match owner(p) {
            Some(app) => (app.clone(), app),
            None => (format!("{}#{}", p.name, p.pid), p.name.clone()),
        };
        let g = groups.entry(key).or_insert(Proc { name, pid: p.pid, mem_bytes: 0, cpu_pct: 0.0 });
        g.mem_bytes += p.mem_bytes;
        g.cpu_pct += p.cpu_pct;
        // 进程号最小的通常是应用的主进程，用它代表整组
        g.pid = g.pid.min(p.pid);
    }
    let mut out: Vec<Proc> = groups.into_values().collect();
    out.sort_by(|a, b| b.mem_bytes.cmp(&a.mem_bytes).then(a.pid.cmp(&b.pid)));
    out
}

/// 进程的（实际占用的内存，累计 CPU 时间 纳秒）。内存包含被系统压缩和换出到磁盘的部分，与「活动监视器」的「内存」一列同口径。
/// 内存吃紧时大户的内存大半被压缩，只看留在物理内存里的部分会小好几倍。读不到（别的用户的进程）时返回 None。
fn usage(pid: u32) -> Option<(u64, u64)> {
    static TIMEBASE: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    // CPU 时间的单位是系统时钟的刻度，Apple 芯片上一刻不是一纳秒
    #[allow(deprecated)]
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut tb = libc::mach_timebase_info { numer: 0, denom: 0 };
        let ok = unsafe { libc::mach_timebase_info(&mut tb) } == 0 && tb.denom != 0;
        if ok { (tb.numer as u64, tb.denom as u64) } else { (1, 1) }
    });
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
    // SAFETY: 输出缓冲区是本函数的局部变量，类型与请求的版本一致。
    let ok = unsafe { libc::proc_pid_rusage(pid as libc::c_int, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) } == 0;
    ok.then(|| {
        let info = unsafe { info.assume_init() };
        let ticks = info.ri_user_time.saturating_add(info.ri_system_time) as u128;
        (info.ri_phys_footprint, (ticks * numer as u128 / denom as u128) as u64)
    })
}

/// 各应用自启动以来的累计耗电（纳焦）和累计写盘（字节），按 `group_processes` 的规则合并到应用，各取前 `n` 个。
/// 只读得到当前用户的进程，系统服务不在里面。
pub fn hogs(n: usize) -> (Vec<Hog>, Vec<Hog>) {
    let mut raw = Vec::new();
    let mut used = Vec::new();
    for pid in all_pids() {
        let Some(exe) = exe_path(pid) else { continue };
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
        // SAFETY: 输出缓冲区是本函数的局部变量，类型与请求的版本一致。
        if unsafe { libc::proc_pid_rusage(pid as libc::c_int, libc::RUSAGE_INFO_V4, info.as_mut_ptr().cast()) } != 0 {
            continue;
        }
        let info = unsafe { info.assume_init() };
        let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        raw.push(RawProc { pid, parent: bsd_info(pid).and_then(|(p, _)| p), responsible: responsible_pid(pid), name, exe: Some(exe), mem_bytes: 0, cpu_pct: 0.0 });
        used.push((info.ri_billed_energy, info.ri_diskio_byteswritten));
    }
    let apps: std::collections::HashSet<String> = raw.iter().filter_map(|p| p.exe.as_deref().and_then(app_of)).collect();
    // ponytail: 借 group_processes 的「归到应用」和排序，把要合计的数临时放进 mem_bytes
    let top = |pick: fn(&(u64, u64)) -> u64| -> Vec<Hog> {
        let with: Vec<RawProc> = raw.iter().zip(&used).map(|(p, u)| RawProc { mem_bytes: pick(u), ..p.clone() }).collect();
        group_processes(&with).into_iter().filter(|g| g.mem_bytes > 0).take(n).map(|g| Hog { app: apps.contains(&g.name), name: g.name, pid: g.pid, value: g.mem_bytes }).collect()
    };
    (top(|u| u.0), top(|u| u.1))
}

/// 全部进程号。
fn all_pids() -> Vec<u32> {
    // SAFETY: 先问个数，多留些余量再取；返回值是实际写入的个数。
    unsafe {
        let n = libc::proc_listallpids(std::ptr::null_mut(), 0);
        if n < 1 {
            return Vec::new();
        }
        let mut pids = vec![0 as libc::c_int; n as usize + 64];
        let got = libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * std::mem::size_of::<libc::c_int>()) as libc::c_int);
        pids.truncate(got.max(0) as usize);
        pids.into_iter().filter(|p| *p > 0).map(|p| p as u32).collect()
    }
}

/// 进程的（父进程，启动时间 微秒）。启动时间用来识别进程号被复用。读不到（别的用户的进程）返回 None。
fn bsd_info(pid: u32) -> Option<(Option<u32>, u64)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: 输出缓冲区是本函数的局部变量，大小与声明一致。
    let got = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    (got == size).then(|| {
        let info = unsafe { info.assume_init() };
        ((info.pbi_ppid != 0).then_some(info.pbi_ppid), info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
    })
}

/// 启动时用的可执行文件路径（不解开符号链接，和以前用的 sysinfo 同口径：`~/.local/bin/claude` 的名字是 claude，
/// 不是它指向的版本号文件）。读不到参数区（别的用户的进程）时退回系统记录的真实路径。
fn exe_path(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let mut size = 0usize;
    // SAFETY: 先问大小再按大小分配；缓冲区和长度都是本函数的局部变量。
    let args = unsafe {
        let mut buf = Vec::new();
        if libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) == 0 && size > 0 {
            buf.resize(size, 0u8);
            if libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) == 0 {
                buf.truncate(size);
            } else {
                buf.clear();
            }
        }
        buf
    };
    // 开头是参数个数（一个 int），紧跟着以 0 结尾的可执行文件路径
    let path = args.get(std::mem::size_of::<libc::c_int>()..).map(|rest| &rest[..rest.iter().position(|b| *b == 0).unwrap_or(rest.len())]);
    if let Some(path) = path.filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(std::ffi::OsStr::from_bytes(path)));
    }
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: 缓冲区大小按系统要求的上限分配；返回值是写入的字节数。
    let n = unsafe { libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr().cast(), buf.len() as u32) };
    (n > 0).then(|| {
        buf.truncate(n as usize);
        PathBuf::from(<std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(buf))
    })
}

/// 一个进程在整个生命期里不变的信息，第一次见到时查一次。
struct Known {
    start: u64,
    name: String,
    exe: PathBuf,
    responsible: Option<u32>,
    /// 上一次采样时的累计 CPU 时间（纳秒）
    cpu_ns: Option<u64>,
}

/// 系统认定的、该为这个进程负责的应用进程。「活动监视器」这类工具靠它把代为启动的服务进程归到应用名下。
/// 这是苹果没有公开的接口，所以运行时再找：哪天系统去掉了它，这里返回 None，合并规则退回只看父进程。
fn responsible_pid(pid: u32) -> Option<u32> {
    type Lookup = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;
    static LOOKUP: std::sync::OnceLock<Option<Lookup>> = std::sync::OnceLock::new();
    let lookup = (*LOOKUP.get_or_init(|| {
        let sym = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"responsibility_get_pid_responsible_for_pid".as_ptr()) };
        (!sym.is_null()).then(|| unsafe { std::mem::transmute::<*mut libc::c_void, Lookup>(sym) })
    }))?;
    let r = unsafe { lookup(pid as libc::pid_t) };
    (r > 0).then_some(r as u32)
}

/// 解析 `pmset -g batt` 的输出；没有电池的机器返回 None。
fn parse_battery(out: &str) -> Option<Battery> {
    let on_ac = out.lines().next()?.contains("AC Power");
    let pct = out.split('%').next()?.rsplit(|c: char| !c.is_ascii_digit()).next()?.parse().ok()?;
    out.contains("InternalBattery").then_some(Battery { pct, charging: on_ac, health_pct: None, cycles: None })
}

/// 内存压力直接取内核自己的分级，不自己定阈值。
fn pressure() -> Pressure {
    let mut level: u32 = 0;
    let mut len = std::mem::size_of::<u32>();
    // SAFETY: 名称是以 NUL 结尾的常量，输出缓冲区和长度都指向本函数的局部变量。
    let ok = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&mut level as *mut u32).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    match (ok, level) {
        (true, 2) => Pressure::Warn,
        (true, 4) => Pressure::Critical,
        _ => Pressure::Normal,
    }
}

/// 交换空间的（已用，总量）。内存不够时系统把页面换到磁盘上，这个数比内存百分比更能看出内存缺多少。
/// 读不到返回 (0, 0)。
fn swap_usage() -> (u64, u64) {
    let mut xsw: libc::xsw_usage = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::xsw_usage>();
    // SAFETY: 名称是以 NUL 结尾的常量，输出缓冲区和长度都指向本函数的局部变量。
    let ok = unsafe {
        libc::sysctlbyname(c"vm.swapusage".as_ptr(), (&mut xsw as *mut libc::xsw_usage).cast(), &mut len, std::ptr::null_mut(), 0)
    } == 0;
    if ok { (xsw.xsu_used, xsw.xsu_total) } else { (0, 0) }
}

/// 已用内存，口径与「活动监视器」的「已使用内存」一致：应用内存 + 联动内存 + 被压缩内存。
/// 读取失败时返回 None，由调用方退回 sysinfo 的数值。
fn mem_used_bytes() -> Option<u64> {
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = (std::mem::size_of::<libc::vm_statistics64>() / std::mem::size_of::<libc::integer_t>()) as u32;
    // SAFETY: stats 和 count 是本函数的局部变量，count 按内核要求以 integer_t 为单位给出缓冲区大小。
    #[allow(deprecated)]
    let ok = unsafe {
        libc::host_statistics64(libc::mach_host_self(), libc::HOST_VM_INFO64, (&mut stats as *mut libc::vm_statistics64).cast(), &mut count)
    } == libc::KERN_SUCCESS;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let app = (stats.internal_page_count as u64).saturating_sub(stats.purgeable_count as u64);
    ok.then(|| (app + stats.wire_count as u64 + stats.compressor_page_count as u64) * page)
}

/// 此刻的空闲内存（空闲页 + 预读页，系统随时能直接拿来用的部分）。「释放内存」前后各量一次，两者之差就是实际腾出了多少：
/// 退出应用腾出的应用内存、清掉的文件缓存都会落到这里。
pub fn free_mem_bytes() -> Option<u64> {
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = (std::mem::size_of::<libc::vm_statistics64>() / std::mem::size_of::<libc::integer_t>()) as u32;
    // SAFETY: 同 mem_used_bytes
    #[allow(deprecated)]
    let ok = unsafe {
        libc::host_statistics64(libc::mach_host_self(), libc::HOST_VM_INFO64, (&mut stats as *mut libc::vm_statistics64).cast(), &mut count)
    } == libc::KERN_SUCCESS;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    ok.then(|| (stats.free_count as u64 + stats.speculative_count as u64) * page)
}

/// 启动盘的容量和已用空间。APFS 下 "/" 报告的是整个容器的数字。
/// 直接问内核，比让 sysinfo 刷新全部磁盘快两个数量级（实测 15 毫秒对不到 0.1 毫秒）。
/// 可用空间和 Finder 同一口径：空闲块再加上 `purgeable`（系统需要时会自动腾出的空间）。
pub(crate) fn startup_disk(purgeable: u64) -> Disk {
    let Some((total, free)) = root_space() else {
        return Disk { used_bytes: 0, total_bytes: 0, health_pct: None };
    };
    Disk { used_bytes: total.saturating_sub(free + purgeable), total_bytes: total, health_pct: None }
}

/// 启动盘的（总容量，空闲块）。
fn root_space() -> Option<(u64, u64)> {
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: 路径是以 NUL 结尾的常量，st 是本函数的局部变量。
    if unsafe { libc::statvfs(c"/".as_ptr(), &mut st) } != 0 {
        return None;
    }
    let unit = st.f_frsize as u64;
    Some((st.f_blocks as u64 * unit, st.f_bavail as u64 * unit))
}

/// 可清除空间 = Finder「可用」减去空闲块。读不到就当 0，退回只算空闲块。
pub(crate) fn purgeable() -> u64 {
    match (finder_free("/"), root_space()) {
        (Some(finder), Some((_, free))) => finder.saturating_sub(free),
        _ => 0,
    }
}

/// Finder「可用」的数字（含 iCloud 已同步文件的本地副本、本地快照等可清除空间）。
fn finder_free(path: &str) -> Option<u64> {
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::number::CFNumber;
    use core_foundation::string::CFStringRef;
    use core_foundation::url::{CFURLRef, CFURL};
    use std::ffi::c_void;
    // core-foundation-sys 没导出这两个（标成了注释），按系统头文件自己声明。
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFURLVolumeAvailableCapacityForImportantUsageKey: CFStringRef;
        fn CFURLCopyResourcePropertyForKey(url: CFURLRef, key: CFStringRef, value: *mut CFTypeRef, error: *mut c_void) -> u8;
    }
    let url = CFURL::from_path(path, true)?;
    let mut value: CFTypeRef = std::ptr::null();
    // SAFETY: url 在调用期间存活；value 是本地变量，拿到的对象按 Copy 规则由 CFType 接管并释放；不要错误详情，传空指针。
    let ok = unsafe {
        CFURLCopyResourcePropertyForKey(url.as_concrete_TypeRef(), kCFURLVolumeAvailableCapacityForImportantUsageKey, &mut value, std::ptr::null_mut())
    } != 0;
    if !ok || value.is_null() {
        return None;
    }
    let value = unsafe { CFType::wrap_under_create_rule(value) };
    value.downcast::<CFNumber>()?.to_i64().filter(|n| *n > 0).map(|n| n as u64)
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 默认路由所在接口的名称，如 "Wi-Fi"。
fn default_iface() -> String {
    let dev = run("/sbin/route", &["-n", "get", "default"])
        .and_then(|o| o.lines().find_map(|l| l.trim().strip_prefix("interface: ").map(str::to_owned)));
    let Some(dev) = dev else { return String::new() };
    if dev.starts_with("utun") || dev.starts_with("ipsec") || dev.starts_with("ppp") {
        return "VPN".into();
    }
    run("/usr/sbin/networksetup", &["-listallhardwareports"])
        .and_then(|o| {
            let mut port = None;
            for l in o.lines() {
                if let Some(p) = l.strip_prefix("Hardware Port: ") {
                    port = Some(p.to_owned());
                } else if l.strip_prefix("Device: ") == Some(dev.as_str()) {
                    return port;
                }
            }
            None
        })
        .unwrap_or(dev)
}

pub fn device_info() -> DeviceInfo {
    let mut sys = System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();
    DeviceInfo {
        chip: sys.cpus().first().map(|c| c.brand().to_owned()).unwrap_or_default(),
        mem_total_bytes: sys.total_memory(),
        os_version: System::os_version().unwrap_or_default(),
    }
}

/// 采样器。CPU 占用和网速都是两次采样之间的差值，所以它必须有状态。
pub struct Sampler {
    sys: System,
    nets: Networks,
    top: Vec<Proc>,
    last: Instant,
    /// 进程号 → 第一次见到时查好的信息
    known: HashMap<u32, Known>,
    procs_at: Instant,
    /// 最近的（CPU 占用，下行网速），旧的在前
    hist: VecDeque<(f32, u64)>,
    battery: Option<(Instant, Option<Battery>)>,
    iface: Option<(Instant, String)>,
    gpu: Option<(Instant, Option<f32>)>,
    health: Option<(Instant, (Option<f32>, Option<u32>))>,
    /// 内置硬盘健康度，和电池健康一起刷新
    ssd_health: Option<u8>,
    purgeable: Option<(Instant, u64)>,
    thermal: crate::sensors::Thermal,
    /// 上次读温度和风扇的时刻
    thermal_at: Option<Instant>,
    /// 上一轮有没有读硬件，用来认出面板刚打开的那一轮
    hw_on: bool,
    hw: Hardware,
}

/// 只在菜单栏面板可见时才读的硬件指标；面板收着时沿用上一次的。
#[derive(Default, Clone)]
struct Hardware {
    cpu_temp: Option<f32>,
    gpu_temp: Option<f32>,
    fan: Option<Fan>,
    mic_in_use: Option<bool>,
    camera_in_use: Option<bool>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        let mut sys = System::new();
        sys.refresh_cpu_usage();
        Self {
            sys,
            nets: Networks::new_with_refreshed_list(),
            top: Vec::new(),
            last: Instant::now(),
            known: HashMap::new(),
            procs_at: Instant::now(),
            hist: VecDeque::with_capacity(HIST_POINTS),
            battery: None,
            iface: None,
            gpu: None,
            health: None,
            ssd_health: None,
            purgeable: None,
            thermal: Default::default(),
            thermal_at: None,
            hw_on: false,
            hw: Hardware::default(),
        }
    }

    /// 刷新全部进程。每个进程每轮只做两次系统调用（父进程和启动时间、内存和 CPU 时间）；
    /// 名字、路径、责任进程只在第一次见到时查。CPU 占用按单核计，可以超过 100%。
    fn processes(&mut self) -> Vec<RawProc> {
        let wall_ns = self.procs_at.elapsed().as_nanos().max(1) as f64;
        self.procs_at = Instant::now();
        let mut old = std::mem::take(&mut self.known);
        let mut raw = Vec::new();
        for pid in all_pids() {
            // 读不到的（别的用户的进程）没有父进程，也认不出进程号复用，和以前用的 sysinfo 一样
            let (parent, start) = bsd_info(pid).unwrap_or((None, 0));
            let known = match old.remove(&pid) {
                Some(k) if k.start == start => k,
                // 新进程，或者进程号被复用了。读不到路径的（已经退出的）不列
                _ => {
                    let Some(exe) = exe_path(pid) else { continue };
                    let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    Known { start, name, exe, responsible: responsible_pid(pid), cpu_ns: None }
                }
            };
            let used = usage(pid);
            let cpu_pct = match (known.cpu_ns, used) {
                (Some(before), Some((_, now))) => (now.saturating_sub(before) as f64 / wall_ns * 100.0) as f32,
                _ => 0.0,
            };
            raw.push(RawProc {
                pid,
                parent,
                responsible: known.responsible,
                name: known.name.clone(),
                exe: Some(known.exe.clone()),
                mem_bytes: used.map_or(0, |(mem, _)| mem),
                cpu_pct,
            });
            self.known.insert(pid, Known { cpu_ns: used.map(|(_, c)| c), ..known });
        }
        raw
    }

    /// `procs` 为 false 时跳过进程列表（沿用上一次的）：遍历近千个进程是采样里较贵的一步，没有窗口可见时没人看。
    /// `hw` 为 false 时跳过温度、风扇、显卡、电池、网卡名这些只有菜单栏面板才显示的指标（沿用上一次的）。
    /// 面板刚打开的那一轮不管有效期，温度、风扇、显卡都重读，免得第一帧显示很久以前的数。
    pub fn sample(&mut self, procs: bool, hw: bool) -> Snapshot {
        let elapsed = self.last.elapsed().as_secs_f64().max(0.001);
        self.last = Instant::now();

        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.nets.refresh(true);

        let (mut down, mut up) = (0u64, 0u64);
        for (name, data) in &self.nets {
            if !name.starts_with("lo") {
                down += data.received();
                up += data.transmitted();
            }
        }

        if procs {
            let raw = self.processes();
            self.top = group_processes(&raw);
            self.top.truncate(TOP_N);
        }

        if hw {
            let opened = !self.hw_on;
            let due = |at: Option<Instant>, ttl: Duration| opened || at.is_none_or(|at| at.elapsed() > ttl);
            if due(self.thermal_at, THERMAL_TTL) {
                self.thermal_at = Some(Instant::now());
                (self.hw.cpu_temp, self.hw.gpu_temp) = crate::sensors::pick(&self.thermal.read());
                let rpms = crate::sensors::fan_rpms();
                self.hw.fan = (!rpms.is_empty()).then(|| Fan { rpm: rpms.iter().copied().max().unwrap_or(0), count: rpms.len() as u32 });
            }
            self.hw.mic_in_use = crate::sensors::mic_in_use();
            self.hw.camera_in_use = crate::sensors::camera_in_use();
            if due(self.gpu.as_ref().map(|(at, _)| *at), GPU_TTL) {
                self.gpu = Some((Instant::now(), crate::sensors::gpu_usage()));
            }
            // 电池健康要跑一百多毫秒的系统报告，几乎不变：面板刚打开那一轮有旧的就先用旧的，不拖慢第一帧，下一轮再补
            match &self.health {
                Some((at, _)) if opened || at.elapsed() <= HEALTH_TTL => {}
                _ => {
                    self.health = Some((Instant::now(), crate::sensors::battery_health()));
                    self.ssd_health = crate::sensors::ssd_health();
                }
            }
            if self.battery.as_ref().is_none_or(|(at, _)| at.elapsed() > BATTERY_TTL) {
                let b = run("/usr/bin/pmset", &["-g", "batt"]).and_then(|o| parse_battery(&o));
                self.battery = Some((Instant::now(), b));
            }
            if self.iface.as_ref().is_none_or(|(at, _)| at.elapsed() > IFACE_TTL) {
                self.iface = Some((Instant::now(), default_iface()));
            }
        }
        self.hw_on = hw;

        // 磁盘快满的提醒要用，窗口都收着时也照读
        if self.purgeable.is_none_or(|(at, _)| at.elapsed() > PURGEABLE_TTL) {
            self.purgeable = Some((Instant::now(), purgeable()));
        }

        let cpu_pct = self.sys.global_cpu_usage().clamp(0.0, 100.0);
        let down_bps = (down as f64 / elapsed) as u64;
        if self.hist.len() == HIST_POINTS {
            self.hist.pop_front();
        }
        self.hist.push_back((cpu_pct, down_bps));

        let swap = swap_usage();
        Snapshot {
            ts_ms: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
            uptime_s: System::uptime(),
            cpu: Cpu {
                usage_pct: cpu_pct,
                load1: System::load_average().one as f32,
                cores: self.sys.cpus().len() as u32,
                temp_c: self.hw.cpu_temp,
            },
            // 读不到就是 None，不编数
            gpu: self.gpu.as_ref().and_then(|(_, usage)| *usage).map(|usage_pct| Gpu { usage_pct, temp_c: self.hw.gpu_temp }),
            mem: Mem {
                used_bytes: mem_used_bytes().unwrap_or_else(|| self.sys.used_memory()).min(self.sys.total_memory()),
                total_bytes: self.sys.total_memory(),
                pressure: pressure(),
                swap_used_bytes: swap.0,
                swap_total_bytes: swap.1,
            },
            disk: Disk { health_pct: self.ssd_health, ..startup_disk(self.purgeable.map_or(0, |(_, p)| p)) },
            net: Net {
                down_bps,
                up_bps: (up as f64 / elapsed) as u64,
                iface: self.iface.as_ref().map(|(_, n)| n.clone()).unwrap_or_default(),
            },
            battery: self.battery.as_ref().and_then(|(_, b)| b.clone()).map(|b| {
                let (health_pct, cycles) = self.health.as_ref().map_or((None, None), |(_, h)| *h);
                Battery { health_pct, cycles, ..b }
            }),
            fan: self.hw.fan.clone(),
            mic_in_use: self.hw.mic_in_use,
            camera_in_use: self.hw.camera_in_use,
            top: self.top.clone(),
            cpu_hist: self.hist.iter().map(|(c, _)| *c).collect(),
            net_hist: self.hist.iter().map(|(_, n)| *n).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 空闲内存读得到且不超过总内存() {
        let free = free_mem_bytes().expect("读得到空闲内存");
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        assert!(free > 0 && free <= sys.total_memory());
    }

    fn raw(pid: u32, parent: Option<u32>, name: &str, exe: Option<&str>, mem: u64, cpu: f32) -> RawProc {
        RawProc { pid, parent, responsible: None, name: name.into(), exe: exe.map(PathBuf::from), mem_bytes: mem, cpu_pct: cpu }
    }

    #[test]
    fn 交换空间读得到() {
        let (used, total) = swap_usage();
        assert!(used <= total);
    }

    #[test]
    fn 启动盘可用空间按_finder_口径读得到() {
        let finder = finder_free("/").expect("读不到 Finder 口径的可用空间");
        let (total, free) = root_space().unwrap();
        // 两次读之间磁盘可能有零星写入，只看量级：Finder 的数不会比空闲块少太多，也不会超过总容量
        assert!(finder + (1 << 30) > free && finder <= total);
        let disk = startup_disk(purgeable());
        assert!(disk.used_bytes < total);
    }

    #[test]
    fn 应用和它的辅助进程合并成一条() {
        let chrome = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
        let helper = "/Applications/Google Chrome.app/Contents/Frameworks/X.framework/Helpers/Google Chrome Helper.app/Contents/MacOS/Google Chrome Helper";
        let procs = vec![
            raw(1, None, "launchd", Some("/sbin/launchd"), 5, 0.0),
            raw(100, Some(1), "Google Chrome", Some(chrome), 400, 2.0),
            raw(101, Some(100), "Google Chrome Helper", Some(helper), 300, 1.5),
            raw(102, Some(100), "Google Chrome Helper", Some(helper), 200, 0.5),
            raw(103, Some(100), "Google Chrome Helper", Some(helper), 100, 1.0),
        ];
        let out = group_processes(&procs);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], Proc { name: "Google Chrome".into(), pid: 100, mem_bytes: 1000, cpu_pct: 5.0 });
        assert_eq!(out[1].name, "launchd");
    }

    #[test]
    fn 不在应用包里的进程归到最近的应用祖先() {
        let term = "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal";
        let procs = vec![
            raw(1, None, "launchd", Some("/sbin/launchd"), 1, 0.0),
            raw(200, Some(1), "Terminal", Some(term), 50, 0.0),
            raw(201, Some(200), "zsh", Some("/bin/zsh"), 10, 0.0),
            raw(202, Some(201), "cargo", Some("/Users/x/.cargo/bin/cargo"), 40, 90.0),
            raw(300, Some(1), "mds", Some("/usr/libexec/mds"), 20, 3.0),
            raw(301, Some(1), "mds", Some("/usr/libexec/mds"), 20, 3.0),
        ];
        let out = group_processes(&procs);
        let terminal = out.iter().find(|p| p.name == "Terminal").unwrap();
        assert_eq!((terminal.mem_bytes, terminal.cpu_pct, terminal.pid), (100, 90.0, 200));
        // 同名但互不相干的系统进程不合并
        assert_eq!(out.iter().filter(|p| p.name == "mds").count(), 2);
    }

    #[test]
    fn 系统代为启动的服务进程归到责任进程所在的应用() {
        let backend = "/Applications/Docker.app/Contents/MacOS/com.docker.backend";
        let vm = "/System/Library/Frameworks/Virtualization.framework/Versions/A/XPCServices/com.apple.Virtualization.VirtualMachine.xpc/Contents/MacOS/com.apple.Virtualization.VirtualMachine";
        let procs = vec![
            raw(1, None, "launchd", Some("/sbin/launchd"), 1, 0.0),
            raw(400, Some(1), "com.docker.backend", Some(backend), 200, 1.0),
            RawProc { responsible: Some(400), ..raw(401, Some(1), "com.apple.Virtualization.VirtualMachine", Some(vm), 800, 30.0) },
            // 责任进程是自己，或者已经不在了：照旧单独列出
            RawProc { responsible: Some(500), ..raw(500, Some(1), "mds", Some("/usr/libexec/mds"), 20, 0.0) },
            RawProc { responsible: Some(999), ..raw(501, Some(1), "mds", Some("/usr/libexec/mds"), 20, 0.0) },
        ];
        let out = group_processes(&procs);
        assert_eq!(out[0], Proc { name: "Docker".into(), pid: 400, mem_bytes: 1000, cpu_pct: 31.0 });
        assert_eq!(out.iter().filter(|p| p.name == "mds").count(), 2);
    }

    #[test]
    fn 应用启动的另一个应用连同它的子进程都算在顶层应用头上() {
        let desktop = "/Applications/Claude.app/Contents/MacOS/Claude";
        let code = "/Users/x/Library/Application Support/Claude/claude-code/2.1/claude.app/Contents/MacOS/claude";
        let procs = vec![
            raw(1, None, "launchd", Some("/sbin/launchd"), 1, 0.0),
            raw(600, Some(1), "Claude", Some(desktop), 300, 1.0),
            // Claude Code 声明了自己负责自己，责任进程不指向桌面端，只能靠父进程找到它
            RawProc { responsible: Some(601), ..raw(601, Some(600), "claude", Some(code), 200, 2.0) },
            RawProc { responsible: Some(601), ..raw(602, Some(601), "node", Some("/usr/local/bin/node"), 100, 3.0) },
        ];
        let out = group_processes(&procs);
        assert_eq!(out[0], Proc { name: "Claude".into(), pid: 600, mem_bytes: 600, cpu_pct: 6.0 });
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn 父子关系成环时不会死循环() {
        let procs = vec![raw(1, Some(2), "a", None, 1, 0.0), raw(2, Some(1), "b", None, 1, 0.0)];
        assert_eq!(group_processes(&procs).len(), 2);
    }

    #[test]
    fn 解析电池状态() {
        let ac = "Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t80%; charging; 0:45 remaining present: true\n";
        assert_eq!(parse_battery(ac), Some(Battery { pct: 80, charging: true, health_pct: None, cycles: None }));
        let batt = "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t7%; discharging; 0:20 remaining present: true\n";
        assert_eq!(parse_battery(batt).map(|b| (b.pct, b.charging)), Some((7, false)));
        assert_eq!(parse_battery("Now drawing from 'AC Power'\n"), None);
    }

    #[test]
    fn 真实采样满足基本不变量() {
        let mut s = Sampler::new();
        let a = s.sample(true, true);
        std::thread::sleep(Duration::from_millis(300));
        let b = s.sample(true, true);
        assert!(b.ts_ms > a.ts_ms);
        assert!((0.0..=100.0).contains(&b.cpu.usage_pct));
        assert!(b.cpu.cores > 0);
        assert!(b.mem.used_bytes <= b.mem.total_bytes && b.mem.total_bytes > 0);
        assert!(b.disk.used_bytes <= b.disk.total_bytes && b.disk.total_bytes > 0);
        assert!(b.top.len() <= TOP_N && !b.top.is_empty());
        let c = s.sample(false, false);
        assert_eq!(c.top, b.top, "精简采样沿用上一次的进程列表");
        assert_eq!((c.cpu_hist.len(), c.net_hist.len()), (3, 3), "窗口都收着时历史也照记");
        assert_eq!(c.cpu_hist[2], c.cpu.usage_pct);
        assert!(b.top.windows(2).all(|w| w[0].mem_bytes >= w[1].mem_bytes));
        if let Some(batt) = &b.battery {
            assert!(batt.pct <= 100);
        }
    }

    #[test]
    fn 历史只留最近的点() {
        let mut s = Sampler::new();
        for _ in 0..HIST_POINTS + 3 {
            s.sample(false, false);
        }
        assert_eq!(s.sample(false, false).cpu_hist.len(), HIST_POINTS);
    }

    #[test]
    fn 自己进程的_cpu_占用按单核计() {
        let me = std::process::id();
        let mut s = Sampler::new();
        let first = s.processes();
        let mine = first.iter().find(|p| p.pid == me).expect("列出了自己");
        assert!(mine.exe.is_some() && !mine.name.is_empty() && mine.mem_bytes > 0);
        assert!(first.len() > 50, "别的用户的进程也列出来（用来沿父进程归组）");
        // 占满一个核 600 毫秒
        let until = Instant::now() + Duration::from_millis(600);
        let mut x = 0u64;
        while Instant::now() < until {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        let cpu = s.processes().into_iter().find(|p| p.pid == me).unwrap().cpu_pct;
        // 其他测试在并行跑，进程总占用会超过 100%；上限只用来抓单位换算错（差几个数量级）
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f32;
        assert!(cpu >= 60.0 && cpu <= cores * 100.0 + 50.0, "单核跑满时至少约 100%，实际 {cpu}");
    }
}
