//! 需要读硬件或系统报告的指标：温度、显卡占用、电池健康。
//! 读不到时一律返回 None，不编数。

use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{kCFAllocatorDefault, CFAllocatorRef, CFType, CFTypeRef, TCFType};
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use std::ffi::c_void;
use std::process::Command;

// Apple 芯片的温度传感器只能通过这组系统内部接口读到（开源的监控工具都是这么做的）。
// 它不是公开接口，系统升级后可能失效，所以每一步都容错，失败就当没有温度。
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDEventSystemClientCreate(allocator: CFAllocatorRef) -> *mut c_void;
    fn IOHIDEventSystemClientSetMatching(client: *mut c_void, matching: CFDictionaryRef) -> i32;
    fn IOHIDEventSystemClientCopyServices(client: *mut c_void) -> CFArrayRef;
    fn IOHIDServiceClientCopyProperty(service: *mut c_void, key: CFStringRef) -> CFTypeRef;
    fn IOHIDServiceClientCopyEvent(service: *mut c_void, kind: i64, options: i32, timestamp: i64) -> *mut c_void;
    fn IOHIDEventGetFloatValue(event: *mut c_void, field: i32) -> f64;
    fn IOServiceMatching(name: *const std::ffi::c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main_port: u32, matching: *mut c_void, iter: *mut u32) -> i32;
    fn IOIteratorNext(iter: u32) -> u32;
    fn IORegistryEntryCreateCFProperty(entry: u32, key: CFStringRef, allocator: CFAllocatorRef, options: u32) -> CFTypeRef;
    fn IOObjectRelease(object: u32) -> i32;
}
extern "C" {
    fn CFRelease(cf: *const c_void);
}

const TEMPERATURE_EVENT: i64 = 15;
const TEMPERATURE_FIELD: i32 = 15 << 16;

/// `pick` 会用到的传感器名字片段。其余（电池、硬盘、外壳等）读了也白读。
const PICKED: [&str; 4] = ["pACC", "eACC", "tdie", "GPU"];

/// 温度读取器。HID 客户端和筛好的传感器列表留着复用：每次重建、逐个问名字要几十毫秒，
/// 而 `pick` 只用其中一小半。持有的是系统对象，只在采样线程里用，不跨线程。
pub struct Thermal {
    client: *mut c_void,
    /// 持有传感器数组，`picked` 里的指针才有效
    services: Option<CFArray<*const c_void>>,
    picked: Vec<(*mut c_void, String)>,
}

impl Default for Thermal {
    fn default() -> Self {
        Self { client: std::ptr::null_mut(), services: None, picked: Vec::new() }
    }
}

impl Thermal {
    /// 重新建客户端，按名字筛出 `pick` 要的传感器。
    fn enumerate(&mut self) {
        self.release();
        // SAFETY: 全部是对系统框架的调用。客户端和传感器数组由本结构持有，在 `release` 里释放；
        // 名字是 Copy 得到的对象，交给包装类型释放。
        unsafe {
            let client = IOHIDEventSystemClientCreate(kCFAllocatorDefault);
            if client.is_null() {
                return;
            }
            self.client = client;
            let matching = CFDictionary::from_CFType_pairs(&[
                (CFString::new("PrimaryUsagePage"), CFNumber::from(0xff00i32)),
                (CFString::new("PrimaryUsage"), CFNumber::from(5i32)),
            ]);
            IOHIDEventSystemClientSetMatching(client, matching.as_concrete_TypeRef());
            let services = IOHIDEventSystemClientCopyServices(client);
            if services.is_null() {
                return;
            }
            let services: CFArray<*const c_void> = CFArray::wrap_under_create_rule(services);
            let key = CFString::new("Product");
            for service in services.iter() {
                let service = *service as *mut c_void;
                let name = IOHIDServiceClientCopyProperty(service, key.as_concrete_TypeRef());
                if name.is_null() {
                    continue;
                }
                if let Some(name) = CFType::wrap_under_create_rule(name).downcast_into::<CFString>().map(|s| s.to_string()) {
                    if PICKED.iter().any(|k| name.contains(k)) {
                        self.picked.push((service, name));
                    }
                }
            }
            self.services = Some(services);
        }
    }

    fn read_picked(&self) -> Vec<(String, f32)> {
        let mut out = Vec::new();
        // SAFETY: 传感器指针来自 `self.services`，数组在本结构存活期间一直有效；事件对象用完即释放。
        unsafe {
            for (service, name) in &self.picked {
                let event = IOHIDServiceClientCopyEvent(*service, TEMPERATURE_EVENT, 0, 0);
                if event.is_null() {
                    continue;
                }
                let celsius = IOHIDEventGetFloatValue(event, TEMPERATURE_FIELD) as f32;
                CFRelease(event);
                // 传感器偶尔会报出离谱的数，只留合理范围内的
                if (1.0..130.0).contains(&celsius) {
                    out.push((name.clone(), celsius));
                }
            }
        }
        out
    }

    /// 处理器和显卡温度的（名称，摄氏度），只含 `pick` 要的那几类。
    /// 一个都读不到时（睡眠唤醒后传感器对象可能换了）重新枚举一次再读，免得温度从此一直为空。
    pub fn read(&mut self) -> Vec<(String, f32)> {
        if self.picked.is_empty() {
            self.enumerate();
            return self.read_picked();
        }
        let out = self.read_picked();
        if !out.is_empty() {
            return out;
        }
        self.enumerate();
        self.read_picked()
    }

    fn release(&mut self) {
        self.picked.clear();
        self.services = None;
        if !self.client.is_null() {
            // SAFETY: 释放自己创建的客户端，之后不再使用。
            unsafe { CFRelease(self.client) };
            self.client = std::ptr::null_mut();
        }
    }
}

impl Drop for Thermal {
    fn drop(&mut self) {
        self.release();
    }
}

/// 从传感器名字里挑出处理器和显卡的温度：各取同类里最热的一个。
pub fn pick(temps: &[(String, f32)]) -> (Option<f32>, Option<f32>) {
    let hottest = |keys: &[&str]| {
        temps.iter().filter(|(n, _)| keys.iter().any(|k| n.contains(k))).map(|(_, t)| *t).reduce(f32::max)
    };
    // 处理器核心的传感器叫 pACC / eACC，较新的机型上叫 tdie；显卡的叫 GPU
    (hottest(&["pACC", "eACC"]).or_else(|| hottest(&["tdie"])), hottest(&["GPU"]))
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 显卡占用百分比。直接问系统注册表（就是 `ioreg -c IOAccelerator` 看到的那一项），不再另起进程。
pub fn gpu_usage() -> Option<f32> {
    let stats_key = CFString::new("PerformanceStatistics");
    let usage_key = CFString::new("Device Utilization %");
    let mut out = None;
    // SAFETY: 标准的 IOKit 遍历流程：匹配字典交给 IOServiceGetMatchingServices 接管，迭代器和每个条目用完即释放；
    // 属性按 Create 规则由 CFType 接管并释放，字典里取出的值按 Get 规则包装。
    unsafe {
        let mut iter = 0u32;
        if IOServiceGetMatchingServices(0, IOServiceMatching(c"IOAccelerator".as_ptr()), &mut iter) != 0 {
            return None;
        }
        loop {
            let entry = IOIteratorNext(iter);
            if entry == 0 {
                break;
            }
            if out.is_none() {
                let stats = IORegistryEntryCreateCFProperty(entry, stats_key.as_concrete_TypeRef(), kCFAllocatorDefault, 0);
                if !stats.is_null() {
                    out = CFType::wrap_under_create_rule(stats)
                        .downcast_into::<CFDictionary>()
                        .and_then(|d| d.find(usage_key.as_CFTypeRef()).map(|v| CFType::wrap_under_get_rule(*v)))
                        .and_then(|v| v.downcast::<CFNumber>()?.to_f64())
                        .map(|v| v as f32);
                }
            }
            IOObjectRelease(entry);
        }
        IOObjectRelease(iter);
    }
    out
}

/// 电池的（健康度百分比，循环次数）。
pub fn battery_health() -> (Option<f32>, Option<u32>) {
    run("/usr/sbin/system_profiler", &["SPPowerDataType"]).map_or((None, None), |t| parse_battery_health(&t))
}

fn parse_battery_health(report: &str) -> (Option<f32>, Option<u32>) {
    let field = |key: &str| report.lines().find_map(|l| l.trim().strip_prefix(key)).map(|v| v.trim().trim_end_matches('%').to_owned());
    (field("Maximum Capacity:").and_then(|v| v.parse().ok()), field("Cycle Count:").and_then(|v| v.parse().ok()))
}

/// 内置固态硬盘的健康度百分比：100 减去 NVMe 健康日志里的「已用寿命」（就是 `smartctl -A disk0` 的 Percentage Used）。
/// 走系统自带的 NVMeSMARTLib 插件（smartmontools 在 Mac 上也是这么读的），不要管理员权限。读不到返回 None。
pub fn ssd_health() -> Option<u8> {
    smart_log().map(|log| health_from_log(&log))
}

/// 内置固态硬盘健康日志里保养页要看的几项。读不到返回 None。
pub fn ssd_smart() -> Option<crate::model::SsdSmart> {
    smart_log().map(|log| smart_from_log(&log))
}

/// 字段位置见 NVMe 规范的 SMART / Health Information 日志页，多字节数都是小端。
fn smart_from_log(log: &[u8; 512]) -> crate::model::SsdSmart {
    let le = |r: std::ops::Range<usize>| log[r].iter().rev().fold(0u128, |a, b| a << 8 | *b as u128);
    // 读写量的单位是一千个 512 字节扇区
    let bytes = |r: std::ops::Range<usize>| (le(r) * 512_000).min(u64::MAX as u128) as u64;
    let kelvin = le(1..3) as i32;
    crate::model::SsdSmart {
        health_pct: health_from_log(log),
        temp_c: (kelvin > 0).then(|| (kelvin - 273) as i16),
        spare_pct: log[3],
        read_bytes: bytes(32..48),
        written_bytes: bytes(48..64),
        power_on_hours: le(128..144) as u64,
        unsafe_shutdowns: le(144..160) as u64,
    }
}

/// NVMe 健康日志第 5 个字节是已用寿命（%），可能超过 100。
fn health_from_log(log: &[u8; 512]) -> u8 {
    100u8.saturating_sub(log[5].min(100))
}

fn smart_log() -> Option<[u8; 512]> {
    use core_foundation::boolean::CFBoolean;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct UuidBytes([u8; 16]);
    type QueryInterface = unsafe extern "C" fn(*mut c_void, UuidBytes, *mut *mut c_void) -> i32;
    type Release = unsafe extern "C" fn(*mut c_void) -> u32;
    /// IOKit 插件接口的开头：COM 风格的 IUnknown 三个函数
    #[repr(C)]
    struct Unknown {
        _reserved: *mut c_void,
        query_interface: QueryInterface,
        _add_ref: Release,
        release: Release,
    }
    /// IONVMeSMARTInterface（见 smartmontools 的 os_darwin.h）：IUnknown 之后是版本号，再后面第一个函数是读健康日志
    #[repr(C)]
    struct SmartInterface {
        base: Unknown,
        _version: u16,
        _revision: u16,
        smart_read_data: unsafe extern "C" fn(*mut c_void, *mut u8) -> i32,
    }
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
        fn IOCreatePlugInInterfaceForService(service: u32, plugin_type: *const c_void, interface_type: *const c_void, plugin: *mut *mut *mut Unknown, score: *mut i32) -> i32;
        fn IODestroyPlugInInterface(plugin: *mut *mut Unknown) -> i32;
    }
    extern "C" {
        fn CFUUIDGetConstantUUIDWithBytes(alloc: CFAllocatorRef, b0: u8, b1: u8, b2: u8, b3: u8, b4: u8, b5: u8, b6: u8, b7: u8, b8: u8, b9: u8, b10: u8, b11: u8, b12: u8, b13: u8, b14: u8, b15: u8) -> *const c_void;
    }
    let uuid = |b: [u8; 16]| unsafe { CFUUIDGetConstantUUIDWithBytes(kCFAllocatorDefault, b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]) };
    // kIONVMeSMARTUserClientTypeID、kIOCFPlugInInterfaceID、kIONVMeSMARTInterfaceID
    const SMART_CLIENT: [u8; 16] = [0xAA, 0x0F, 0xA6, 0xF9, 0xC2, 0xD6, 0x45, 0x7F, 0xB1, 0x0B, 0x59, 0xA1, 0x32, 0x53, 0x29, 0x2F];
    const PLUGIN: [u8; 16] = [0xC2, 0x44, 0xE8, 0x58, 0x10, 0x9C, 0x11, 0xD4, 0x91, 0xD4, 0x00, 0x50, 0xE4, 0xC6, 0x42, 0x6F];
    const SMART_INTERFACE: [u8; 16] = [0xCC, 0xD1, 0xDB, 0x19, 0xFD, 0x9A, 0x4D, 0xAF, 0xBF, 0x95, 0x12, 0x45, 0x4B, 0x23, 0x0A, 0xB6];

    // 找声明了「NVMe SMART Capable」的那块盘；Mac 的内置盘只有一块
    let capable = CFDictionary::from_CFType_pairs(&[(CFString::new("NVMe SMART Capable"), CFBoolean::true_value().as_CFType())]);
    let matching = CFDictionary::from_CFType_pairs(&[(CFString::new("IOPropertyMatch"), capable.as_CFType())]);
    let mut log = [0u8; 512];
    // SAFETY: 匹配字典按 IOKit 约定被 IOServiceGetMatchingService 接管，所以先多留一份引用；
    // 插件和接口都是指向函数表指针的指针，按 IUnknown 约定用完 Release / Destroy，服务对象用完释放。
    unsafe {
        let matching_ref = matching.as_concrete_TypeRef();
        core_foundation::base::CFRetain(matching_ref as CFTypeRef);
        let service = IOServiceGetMatchingService(0, matching_ref as *mut c_void);
        if service == 0 {
            return None;
        }
        let (mut plugin, mut score) = (std::ptr::null_mut(), 0);
        let made = IOCreatePlugInInterfaceForService(service, uuid(SMART_CLIENT), uuid(PLUGIN), &mut plugin, &mut score);
        IOObjectRelease(service);
        if made != 0 || plugin.is_null() {
            return None;
        }
        let mut smart: *mut *mut SmartInterface = std::ptr::null_mut();
        let found = ((**plugin).query_interface)(plugin.cast(), UuidBytes(SMART_INTERFACE), (&mut smart as *mut *mut *mut SmartInterface).cast());
        let ok = found == 0 && !smart.is_null() && ((**smart).smart_read_data)(smart.cast(), log.as_mut_ptr()) == 0;
        if !smart.is_null() {
            ((**smart).base.release)(smart.cast());
        }
        IODestroyPlugInInterface(plugin);
        ok.then_some(log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 硬盘健康度是一百减已用寿命() {
        let mut log = [0u8; 512];
        log[5] = 12;
        assert_eq!(health_from_log(&log), 88);
        log[5] = 130;
        assert_eq!(health_from_log(&log), 0);
        log[5] = 3;
        log[1..3].copy_from_slice(&310u16.to_le_bytes());
        log[3] = 100;
        log[48..56].copy_from_slice(&2_000_000u64.to_le_bytes());
        log[128] = 200;
        log[129] = 1;
        let s = smart_from_log(&log);
        assert_eq!((s.health_pct, s.temp_c, s.spare_pct, s.written_bytes, s.power_on_hours), (97, Some(37), 100, 1_024_000_000_000, 456));
        if let Some(s) = ssd_smart() {
            println!("本机硬盘 {s:?}");
        }
        // 本机能读到时，数字要在范围内（读不到的机器上跳过）
        if let Some(h) = ssd_health() {
            println!("本机硬盘健康度 {h}%");
            assert!(h <= 100);
        }
    }

    #[test]
    fn 按名字挑出处理器和显卡温度() {
        let t = |n: &str, c: f32| (n.to_owned(), c);
        let temps = [t("pACC MTR Temp Sensor0", 51.0), t("eACC MTR Temp Sensor3", 47.5), t("GPU MTR Temp Sensor1", 44.0), t("NAND CH0 temp", 33.0), t("gas gauge battery", 30.0)];
        assert_eq!(pick(&temps), (Some(51.0), Some(44.0)));
        assert_eq!(pick(&[t("PMU tdie1", 40.0), t("PMU tdie2", 42.0)]), (Some(42.0), None));
        assert_eq!(pick(&[]), (None, None));
    }

    #[test]
    fn 解析电池健康() {
        let report = "      Health Information:\n          Cycle Count: 219\n          Condition: Normal\n          Maximum Capacity: 89%\n";
        assert_eq!(parse_battery_health(report), (Some(89.0), Some(219)));
        assert_eq!(parse_battery_health(""), (None, None));
    }

    #[test]
    fn 显卡占用和_ioreg_看到的一致() {
        let ioreg = run("/usr/sbin/ioreg", &["-r", "-d", "1", "-c", "IOAccelerator"]).unwrap_or_default();
        let usage = gpu_usage();
        assert_eq!(usage.is_some(), ioreg.contains("\"Device Utilization %\""));
        assert!(usage.is_none_or(|u| (0.0..=100.0).contains(&u)));
    }

    #[test]
    fn 温度读取器只留要用的传感器且能反复读() {
        let mut thermal = Thermal::default();
        let first = thermal.read();
        assert!(first.iter().all(|(n, _)| PICKED.iter().any(|k| n.contains(k))));
        // 复用同一个客户端再读一次，读到的传感器还是那些
        let again = thermal.read();
        assert_eq!(first.iter().map(|(n, _)| n).collect::<Vec<_>>(), again.iter().map(|(n, _)| n).collect::<Vec<_>>());
    }
}

// ---- 风扇转速：从系统管理控制器读取。只读，不需要管理员权限。 ----

mod smc {
    use std::ffi::{c_char, c_void};

    /// 与系统头文件里的结构逐字段对应，共 80 字节。
    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct KeyData {
        key: u32,
        vers: [u8; 6],
        _pad_after_vers: [u8; 2],
        p_limit: [u8; 16],
        data_size: u32,
        data_type: u32,
        data_attributes: u8,
        _pad_after_key_info: [u8; 3],
        result: u8,
        status: u8,
        data8: u8,
        data32: u32,
        bytes: [u8; 32],
    }

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> u32;
        fn IOServiceOpen(service: u32, owning_task: u32, kind: u32, connect: *mut u32) -> i32;
        fn IOServiceClose(connect: u32) -> i32;
        fn IOObjectRelease(object: u32) -> i32;
        fn IOConnectCallStructMethod(connect: u32, selector: u32, input: *const c_void, input_size: usize, output: *mut c_void, output_size: *mut usize) -> i32;
    }
    extern "C" {
        static mach_task_self_: u32;
    }

    const HANDLE_EVENT: u32 = 2;
    const CMD_READ_BYTES: u8 = 5;
    const CMD_READ_KEY_INFO: u8 = 9;

    pub struct Connection(u32);

    impl Connection {
        pub fn open() -> Option<Self> {
            // SAFETY: 标准的 IOKit 打开流程；服务对象用完即释放，连接在 Drop 里关闭。
            unsafe {
                let service = IOServiceGetMatchingService(0, IOServiceMatching(c"AppleSMC".as_ptr()));
                if service == 0 {
                    return None;
                }
                let mut conn = 0;
                let ok = IOServiceOpen(service, mach_task_self_, 0, &mut conn) == 0;
                IOObjectRelease(service);
                ok.then_some(Self(conn))
            }
        }

        fn call(&self, input: &KeyData) -> Option<KeyData> {
            let mut out = KeyData::default();
            let mut size = std::mem::size_of::<KeyData>();
            // SAFETY: 输入输出都是本函数持有的、大小正确的结构。
            let ok = unsafe {
                IOConnectCallStructMethod(self.0, HANDLE_EVENT, (input as *const KeyData).cast(), size, (&mut out as *mut KeyData).cast(), &mut size)
            } == 0;
            (ok && out.result == 0).then_some(out)
        }

        /// 读一个四字符的键，返回（类型，原始字节）。
        pub fn read(&self, key: &[u8; 4]) -> Option<([u8; 4], Vec<u8>)> {
            let key = u32::from_be_bytes(*key);
            let info = self.call(&KeyData { key, data8: CMD_READ_KEY_INFO, ..Default::default() })?;
            let data = self.call(&KeyData { key, data_size: info.data_size, data8: CMD_READ_BYTES, ..Default::default() })?;
            Some((info.data_type.to_be_bytes(), data.bytes[..(info.data_size as usize).min(32)].to_vec()))
        }
    }

    impl Drop for Connection {
        fn drop(&mut self) {
            // SAFETY: 关闭自己打开的连接。
            unsafe { IOServiceClose(self.0) };
        }
    }

    const _: () = assert!(std::mem::size_of::<KeyData>() == 80);
}

/// 把读到的原始字节按类型换成转速。Apple 芯片上是小端浮点数，Intel 上是 14.2 定点数。
fn decode_rpm(kind: &[u8; 4], bytes: &[u8]) -> Option<f32> {
    match (kind, bytes) {
        (b"flt ", [a, b, c, d, ..]) => Some(f32::from_le_bytes([*a, *b, *c, *d])),
        (b"fpe2", [a, b, ..]) => Some(u16::from_be_bytes([*a, *b]) as f32 / 4.0),
        _ => None,
    }
}

/// 各个风扇当前的转速。没有风扇的机型返回空。
pub fn fan_rpms() -> Vec<u32> {
    let Some(conn) = smc::Connection::open() else { return vec![] };
    let count = conn.read(b"FNum").and_then(|(_, b)| b.first().copied()).unwrap_or(0).min(8);
    (0..count)
        .filter_map(|i| {
            let (kind, bytes) = conn.read(&[b'F', b'0' + i, b'A', b'c'])?;
            decode_rpm(&kind, &bytes).filter(|r| (0.0..20000.0).contains(r)).map(|r| r.round() as u32)
        })
        .collect()
}

#[cfg(test)]
mod fan_tests {
    use super::*;

    #[test]
    fn 两种芯片的转速编码() {
        assert_eq!(decode_rpm(b"flt ", &1363.0f32.to_le_bytes()), Some(1363.0));
        assert_eq!(decode_rpm(b"fpe2", &[0x15, 0x4c]), Some(1363.0));
        assert_eq!(decode_rpm(b"ui8 ", &[1]), None);
    }
}

// ---- 麦克风和摄像头是否正在被使用。只读系统的公开状态，不会触发权限询问，也不会打开设备。 ----

mod media {
    use std::ffi::c_void;

    #[repr(C)]
    struct Address {
        selector: u32,
        scope: u32,
        element: u32,
    }

    const fn fourcc(s: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*s)
    }
    const SYSTEM_OBJECT: u32 = 1;
    const SCOPE_GLOBAL: u32 = fourcc(b"glob");
    const RUNNING_SOMEWHERE: u32 = fourcc(b"gone");
    const DEFAULT_INPUT: u32 = fourcc(b"dIn ");
    const DEVICES: u32 = fourcc(b"dev#");

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyData(object: u32, address: *const Address, qualifier_size: u32, qualifier: *const c_void, size: *mut u32, data: *mut c_void) -> i32;
    }
    #[link(name = "CoreMediaIO", kind = "framework")]
    extern "C" {
        fn CMIOObjectGetPropertyDataSize(object: u32, address: *const Address, qualifier_size: u32, qualifier: *const c_void, size: *mut u32) -> i32;
        fn CMIOObjectGetPropertyData(object: u32, address: *const Address, qualifier_size: u32, qualifier: *const c_void, size: u32, used: *mut u32, data: *mut c_void) -> i32;
    }

    fn audio_u32(object: u32, selector: u32) -> Option<u32> {
        let addr = Address { selector, scope: SCOPE_GLOBAL, element: 0 };
        let (mut value, mut size) = (0u32, 4u32);
        // SAFETY: 输出缓冲区是本函数的局部变量，大小与声明一致。
        let ok = unsafe { AudioObjectGetPropertyData(object, &addr, 0, std::ptr::null(), &mut size, (&mut value as *mut u32).cast()) } == 0;
        ok.then_some(value)
    }

    /// 默认的输入设备有没有被任何程序打开。
    pub fn mic_in_use() -> Option<bool> {
        let device = audio_u32(SYSTEM_OBJECT, DEFAULT_INPUT).filter(|d| *d != 0)?;
        audio_u32(device, RUNNING_SOMEWHERE).map(|v| v != 0)
    }

    /// 任意一个摄像头有没有被任何程序打开。
    pub fn camera_in_use() -> Option<bool> {
        let addr = Address { selector: DEVICES, scope: SCOPE_GLOBAL, element: 0 };
        let mut size = 0u32;
        // SAFETY: 先问大小再按大小分配缓冲区；设备编号数组在本函数内使用。
        unsafe {
            if CMIOObjectGetPropertyDataSize(SYSTEM_OBJECT, &addr, 0, std::ptr::null(), &mut size) != 0 {
                return None;
            }
            let mut devices = vec![0u32; (size / 4) as usize];
            let mut used = 0u32;
            if !devices.is_empty() && CMIOObjectGetPropertyData(SYSTEM_OBJECT, &addr, 0, std::ptr::null(), size, &mut used, devices.as_mut_ptr().cast()) != 0 {
                return None;
            }
            let running = Address { selector: RUNNING_SOMEWHERE, scope: SCOPE_GLOBAL, element: 0 };
            Some(devices.iter().take((used / 4) as usize).any(|d| {
                let (mut value, mut got) = (0u32, 0u32);
                CMIOObjectGetPropertyData(*d, &running, 0, std::ptr::null(), 4, &mut got, (&mut value as *mut u32).cast()) == 0 && value != 0
            }))
        }
    }
}

pub use media::{camera_in_use, mic_in_use};
