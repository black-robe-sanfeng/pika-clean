//! 前后端之间传递的数据模型。改这里必须同步 `frontend/src/api/types.ts` 和 `docs/api/README.md`。

use serde::Serialize;

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub ts_ms: u64,
    pub uptime_s: u64,
    pub cpu: Cpu,
    pub gpu: Option<Gpu>,
    pub mem: Mem,
    pub disk: Disk,
    pub net: Net,
    pub battery: Option<Battery>,
    pub fan: Option<Fan>,
    /// 麦克风、摄像头此刻有没有被任何程序打开；读不到时为空
    pub mic_in_use: Option<bool>,
    pub camera_in_use: Option<bool>,
    /// 按内存降序，最多 `metrics::TOP_N` 条
    pub top: Vec<Proc>,
    /// 最近 `metrics::HIST_POINTS` 次采样的 CPU 占用和下行网速，旧的在前。没有窗口可见时也照样记（那时 5 秒一次）
    pub cpu_hist: Vec<f32>,
    pub net_hist: Vec<u64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Cpu {
    /// 0–100，全部核心的平均值
    pub usage_pct: f32,
    pub load1: f32,
    pub cores: u32,
    pub temp_c: Option<f32>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Gpu {
    pub usage_pct: f32,
    pub temp_c: Option<f32>,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Pressure {
    Normal,
    Warn,
    Critical,
}

#[derive(Serialize, Clone, Debug)]
pub struct Mem {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub pressure: Pressure,
    /// 交换空间（`vm.swapusage`）。两个都是 0 表示没有交换文件或读不到。
    pub swap_used_bytes: u64,
    pub swap_total_bytes: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Disk {
    pub used_bytes: u64,
    pub total_bytes: u64,
    /// 内置固态硬盘的健康度（100 减已用寿命）；读不到、或面板没打开过时为空
    pub health_pct: Option<u8>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Net {
    pub down_bps: u64,
    pub up_bps: u64,
    /// 默认路由所在的网络接口名称，如 "Wi-Fi"；没有网络时为空
    pub iface: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Battery {
    pub pct: u8,
    /// 是否接着电源
    pub charging: bool,
    pub health_pct: Option<f32>,
    pub cycles: Option<u32>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Fan {
    /// 转得最快的那个风扇的转速；风扇停转时为 0
    pub rpm: u32,
    pub count: u32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Proc {
    pub name: String,
    pub pid: u32,
    pub mem_bytes: u64,
    /// 按单核 100% 计，与「活动监视器」一致，可以超过 100
    pub cpu_pct: f32,
}

#[derive(Serialize, Clone, Debug)]
pub struct DeviceInfo {
    pub chip: String,
    pub mem_total_bytes: u64,
    pub os_version: String,
}

// ---- 保养 Mac ----

/// 保养页的体检结果。全部只读，读不到的项为空。
#[derive(Serialize, Clone, Debug)]
pub struct CareReport {
    /// 台式机没有电池时为空
    pub battery: Option<BatteryCare>,
    pub disk: DiskCare,
    /// 各应用自启动以来累计耗电最多的几个，值是纳焦；读不到能耗（部分 Intel 机型）时为空列表
    pub energy: Vec<Hog>,
    /// 各应用自启动以来累计写盘最多的几个，值是字节
    pub writes: Vec<Hog>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct BatteryCare {
    /// 和「系统设置 › 电池 › 电池健康」同一口径
    pub health_pct: Option<f32>,
    pub cycles: Option<u32>,
    /// 设计寿命的循环次数，Apple 笔记本一般是 1000
    pub design_cycles: Option<u32>,
    pub design_mah: Option<u32>,
    /// 现在充满能装多少
    pub max_mah: Option<u32>,
    pub temp_c: Option<f32>,
    pub pct: Option<u8>,
    /// 是否接着电源
    pub plugged: bool,
    pub charging: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct DiskCare {
    pub used_bytes: u64,
    pub total_bytes: u64,
    /// 内置固态硬盘的健康日志；外接盘、读不到时为空
    pub smart: Option<SsdSmart>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct SsdSmart {
    /// 100 减已用寿命
    pub health_pct: u8,
    pub temp_c: Option<i16>,
    /// 备用块还剩多少，低于 10% 要尽快备份
    pub spare_pct: u8,
    pub read_bytes: u64,
    pub written_bytes: u64,
    pub power_on_hours: u64,
    /// 没正常关机（强制关机、断电）的次数
    pub unsafe_shutdowns: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Hog {
    pub name: String,
    pub pid: u32,
    pub value: u64,
    /// 是不是 `.app` 应用；只有应用才给「退出」按钮，系统服务退了也会被拉起来
    pub app: bool,
}

// ---- 扫描与删除 ----

fn lossy<S: serde::Serializer>(p: &std::path::Path, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&p.to_string_lossy())
}

#[derive(Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CategoryId {
    AppCache,
    SystemCache,
    Trash,
    OldDownloads,
    Logs,
    /// 开发工具的缓存：包管理器下载的包、Xcode 派生数据、模拟器缓存等
    DevCache,
    /// 很久没动过的项目里可以重新生成的依赖和构建目录（node_modules、target 等）
    StaleProjects,
    /// AI 编程工具（Claude、Codex、OpenCode、Cursor 等）可以重新生成的缓存和日志
    AiCache,
    /// AI 编程工具很久没动过的对话记录和改动快照
    AiHistory,
    /// 程序崩溃时系统写在 `/cores` 下的内存转储（`core.<进程号>`）
    CrashDumps,
    /// iPhone、iPad 在这台 Mac 上的本地备份，一份备份一个文件夹。个人数据，只进废纸篓
    IosBackups,
    /// Xcode 打包归档（`.xcarchive`，含还原崩溃日志要用的 dSYM）。个人数据，只进废纸篓
    XcodeArchives,
    /// 给 iPhone、iPad 刷机下载的系统固件（`.ipsw`），可以重新下载
    IosFirmware,
    /// 「邮件」打开过的附件的副本
    MailDownloads,
    /// `brew cleanup -n` 说可以删的 Homebrew 旧版本
    HomebrewOld,
    /// 下载、桌面、文稿里内容完全相同的文件：每组留一份，其余列出来
    Duplicates,
    /// 个人文件夹里的大文件和很久没打开的文件
    LargeFiles,
    /// 浏览器的浏览记录和 Cookie（Safari、Chrome、Edge、Brave）。删了会退出各网站的登录：默认不勾选，只进废纸篓
    BrowserPrivacy,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Item {
    /// 稳定编号：同一路径两次扫描得到相同编号。界面和 AI 只用它指代条目，不传路径。
    pub id: String,
    #[serde(serialize_with = "lossy")]
    pub path: std::path::PathBuf,
    pub size_bytes: u64,
    pub last_used_ms: Option<u64>,
    /// 在时限内没量完，`size_bytes` 只是下限
    pub partial: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Category {
    pub id: CategoryId,
    #[serde(serialize_with = "lossy")]
    pub root: std::path::PathBuf,
    pub total_bytes: u64,
    /// 按大小降序
    pub items: Vec<Item>,
    pub default_selected: bool,
    /// 这一类的删除方式，由 `remove::mode_of` 决定，界面在确认单里照此说明
    pub removal: crate::remove::Mode,
    pub needs_admin: bool,
    pub needs_full_disk_access: bool,
    /// 因权限不足读不到。此时 items 为空。
    pub unreadable: bool,
    /// 有条目在时限内没量完，`total_bytes` 只是下限
    pub partial: bool,
    /// 这一类只可能包含扫描位置下的这几个固定路径（相对路径）。为空表示「扫描位置的每个一级子项」。
    /// 删除前的守卫靠它判断条目是不是规则里写明的那几个。
    #[serde(skip)]
    pub only: Vec<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct ScanReport {
    pub categories: Vec<Category>,
    /// 扫描开始的时间（毫秒时间戳），界面上显示「扫描于」
    pub at_ms: u64,
}

#[derive(Serialize, Clone, Debug)]
pub struct Progress {
    pub category: CategoryId,
    /// 已完成的类别数
    pub done: u32,
    pub total: u32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Failure {
    #[serde(serialize_with = "lossy")]
    pub path: std::path::PathBuf,
    pub reason: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct RemoveResult {
    /// 直接删除实际释放的空间
    pub freed_bytes: u64,
    /// 移到废纸篓的大小，清空废纸篓后才释放
    pub trashed_bytes: u64,
    pub failed: Vec<Failure>,
    /// `freed_bytes` 是估计值（开发环境清理后读不到实际占用时，或删了开发工具缓存：里面的文件可能和别处共用数据块），界面写「大约」
    pub estimated: bool,
    /// 这次移到废纸篓的条目和它们在废纸篓里的位置，界面用来「放回原处」
    pub trashed: Vec<Trashed>,
    /// 清理已经做完、但有别的事没办成（比如清理记录没写进去）时给用户看的提示
    pub warning: Option<String>,
}

/// 移到废纸篓的一项：原来在哪、现在在废纸篓的哪里（同名时系统会改名，比如「node_modules 2」）。
#[derive(Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Trashed {
    #[serde(serialize_with = "lossy")]
    pub from: std::path::PathBuf,
    #[serde(serialize_with = "lossy")]
    pub to: std::path::PathBuf,
    pub size_bytes: u64,
}

/// 清理进度：处理到第几项。`admin` 为 true 时在等用户在系统密码框里输密码。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct CleanProgress {
    pub done: u32,
    pub total: u32,
    pub admin: bool,
}

// ---- 清理记录与系统小工具 ----

/// 累计做过多少事。显示在菜单栏面板底部。
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct JournalSummary {
    /// 清理掉的总大小（直接删除的加上移到废纸篓的）
    pub cleaned_bytes: u64,
    pub uninstalled: u64,
    pub optimized: u64,
}

// ---- 卸载软件 ----

/// 卸载进度：正在处理第几个应用；`waiting` 为 true 时在等它退出（最多 10 秒）。
#[derive(Serialize, Clone, Debug)]
pub struct UninstallProgress {
    pub name: String,
    pub done: u32,
    pub total: u32,
    pub waiting: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Leftover {
    #[serde(serialize_with = "lossy")]
    pub path: std::path::PathBuf,
    /// 还没量过时为空。量残留要遍历目录，所以只在用户选中这个应用时才量。
    pub size_bytes: Option<u64>,
}

/// 认得是这个应用的、但卸载时不动的东西，只列出来给用户看。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Kept {
    #[serde(serialize_with = "lossy")]
    pub path: std::path::PathBuf,
    /// 和哪个仍装着的应用共用（同一包标识、同名或同一应用组）；为空表示在 `/Library` 下、要管理员权限才能删，本应用不删
    pub shared_with: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct AppInfo {
    /// 应用的编号。卸载时前端只传它。
    pub id: String,
    pub name: String,
    pub bundle_id: String,
    pub version: String,
    #[serde(serialize_with = "lossy")]
    pub path: std::path::PathBuf,
    pub size_bytes: u64,
    pub last_used_ms: Option<u64>,
    /// 应用包的创建时间，大致是装上的时间；读不到时为空
    pub created_ms: Option<u64>,
    /// 应用在资源库里留下的缓存、偏好设置等
    pub leftovers: Vec<Leftover>,
    /// 残留文件的总大小；还没量过时为空
    pub leftover_bytes: Option<u64>,
    /// 为 false 表示在时限内没量完，`leftover_bytes` 只是下限
    pub leftover_complete: bool,
    /// 卸载时保留的
    pub kept: Vec<Kept>,
    /// 正在运行的应用不能卸载
    pub running: bool,
    /// 主程序之外、包里正在运行的进程名（比如 Xcode 里的 git、clang）。卸载前也会请它们退出
    pub tools_running: Vec<String>,
}
