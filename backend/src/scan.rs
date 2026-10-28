//! 扫描：按固定规则找出可清理的文件。只读，不删任何东西。

use crate::model::{Category, CategoryId, Item, Progress, ScanReport};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 系统自己放的占位文件，不算可清理条目。
const SKIP_NAMES: [&str; 2] = [".DS_Store", ".localized"];
pub const OLD_DOWNLOAD_DAYS: u64 = 90;

pub struct Rule {
    pub id: CategoryId,
    pub root: PathBuf,
    pub default_selected: bool,
    pub needs_admin: bool,
    pub needs_full_disk_access: bool,
    /// 只列出超过这么多天没用过的条目
    pub older_than_days: Option<u64>,
    /// 名称以这些前缀开头的条目不列出
    pub skip_prefixes: &'static [&'static str],
    /// 不为空时，这一类只包含扫描位置下的这几个固定路径，而不是每个一级子项
    pub only: &'static [&'static str],
}

/// 开发工具的缓存目录（相对用户主目录）。都是可以重新下载或重新生成的。
/// `~/Library/Caches` 下面的（pip、Yarn、Homebrew 等）已经算在「应用缓存」里，这里不重复。
/// pnpm、uv 在 APFS 上用克隆把文件导入各项目，和项目里的副本共用数据块，删掉后实际释放的空间可能比显示的少。
pub const DEV_CACHE_DIRS: [&str; 16] = [
    ".npm/_cacache",
    "Library/pnpm/store",
    ".cache/uv",
    "go/pkg/mod",
    ".cache/yarn",
    ".gradle/caches",
    ".cargo/registry/cache",
    ".m2/repository",
    ".pub-cache/hosted",
    ".cocoapods/repos",
    "Library/Developer/CoreSimulator/Caches",
    "Library/Developer/Xcode/iOS DeviceSupport",
    "Library/Developer/Xcode/watchOS DeviceSupport",
    "Library/Developer/Xcode/tvOS DeviceSupport",
    "Library/Developer/Xcode/visionOS DeviceSupport",
    "Library/Developer/Xcode/DerivedData",
];

/// AI 编程工具的缓存和日志（相对用户主目录），删了会自动重新生成。
/// `vm_bundles` 是 Claude 桌面版的虚拟机镜像，删了下次用到时会重新下载，所以这一类默认不勾选。
pub const AI_CACHE_DIRS: [&str; 15] = [
    "Library/Caches/claude-cli-nodejs",
    ".claude/shell-snapshots",
    ".claude/paste-cache",
    "Library/Application Support/Claude/Cache",
    "Library/Application Support/Claude/Code Cache",
    "Library/Application Support/Claude/vm_bundles",
    ".codex/cache",
    ".codex/log",
    ".local/share/opencode/log",
    ".cache/opencode",
    ".gemini/tmp",
    "Library/Application Support/Cursor/Cache",
    "Library/Application Support/Cursor/CachedData",
    "Library/Application Support/Cursor/Code Cache",
    "Library/Application Support/Cursor/logs",
];

/// AI 编程工具存对话记录的位置（相对用户主目录）和条目所在的层数：
/// 一个条目就是一次会话（或一天的会话），删了就不能再接着那次对话聊。
/// Claude Code 每个项目目录下的 `memory` 是它的长期记忆，不是对话记录，不列出。
pub const AI_HISTORY_ROOTS: [(&str, u32); 5] = [
    (".claude/projects", 2),
    (".claude/file-history", 1),
    (".codex/sessions", 3),
    (".codex/archived_sessions", 1),
    (".local/share/opencode/snapshot", 1),
];
const AI_HISTORY_SKIP: [&str; 1] = ["memory"];
/// 超过这么多天没改动的会话才列出，最近的留着方便接着聊。
pub const AI_HISTORY_DAYS: u64 = 30;

/// 在各个对话记录位置里，找出指定层数上超过 `days` 天没改动的条目。只读，不跟随符号链接。
pub(crate) fn find_ai_history(home: &Path, now: SystemTime, days: u64) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for (rel, depth) in AI_HISTORY_ROOTS {
        let mut level = vec![home.join(rel)];
        for d in 1..=depth {
            let mut next = Vec::new();
            for dir in level {
                for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
                    let name = e.file_name();
                    let Ok(meta) = e.path().symlink_metadata() else { continue };
                    if SKIP_NAMES.iter().chain(&AI_HISTORY_SKIP).any(|s| name == *s) || meta.file_type().is_symlink() {
                        continue;
                    }
                    if d < depth {
                        if meta.is_dir() {
                            next.push(e.path());
                        }
                    } else if meta.modified().ok().and_then(|m| now.duration_since(m).ok()).is_some_and(|idle| idle >= Duration::from_secs(days * 86400)) {
                        found.push(e.path());
                    }
                }
            }
            level = next;
        }
    }
    found
}

/// 这些目录里的文件可能有几十万个，每个最多量这么久，量不完就标「至少」。
const ONLY_ITEM_BUDGET: Duration = Duration::from_secs(4);
/// 「每个一级子项」的类别（应用缓存、废纸篓、日志等）量全部条目最多这么久，量不完就标「至少」。
const LIST_SIZE_BUDGET: Duration = Duration::from_secs(20);
/// 同时扫的类别数。空间去向那边实测 4 个线程以上就不再明显变快。
const SCAN_THREADS: usize = 4;
/// 一次扫描最多这么久：各类自己的时限都不会超过它，到了还没量完的标「至少」。
const SCAN_BUDGET: Duration = Duration::from_secs(60);

/// 一次扫描共用的总时限和取消标志。到了时限或被取消，各处马上停手，已经找到、量到的照常返回。
pub struct Stop<'a> {
    deadline: Instant,
    cancel: &'a AtomicBool,
}

impl Stop<'_> {
    /// 时限取「从现在起 `budget`」和总时限里早的那个
    fn within(&self, budget: Duration) -> Stop<'_> {
        Stop { deadline: self.deadline.min(Instant::now() + budget), cancel: self.cancel }
    }
    fn hit(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || Instant::now() >= self.deadline
    }
}

/// iPhone、iPad 系统固件（`.ipsw`）的下载位置（相对用户主目录）。
/// 浏览器的浏览记录和 Cookie 文件（相对主目录），连同 SQLite 的日志文件一起：只删主文件留下日志，下次打开可能把旧记录写回去。
/// 缓存不在这里，已经归「应用缓存」。Firefox 的配置目录名是随机的，没列。
pub const BROWSER_PRIVACY_FILES: [&str; 18] = [
    "Library/Safari/History.db",
    "Library/Safari/History.db-wal",
    "Library/Safari/History.db-shm",
    "Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies",
    "Library/Application Support/Google/Chrome/Default/History",
    "Library/Application Support/Google/Chrome/Default/History-journal",
    "Library/Application Support/Google/Chrome/Default/Cookies",
    "Library/Application Support/Google/Chrome/Default/Cookies-journal",
    "Library/Application Support/Google/Chrome/Default/Network/Cookies",
    "Library/Application Support/Google/Chrome/Default/Network/Cookies-journal",
    "Library/Application Support/Microsoft Edge/Default/History",
    "Library/Application Support/Microsoft Edge/Default/History-journal",
    "Library/Application Support/Microsoft Edge/Default/Cookies",
    "Library/Application Support/Microsoft Edge/Default/Cookies-journal",
    "Library/Application Support/BraveSoftware/Brave-Browser/Default/History",
    "Library/Application Support/BraveSoftware/Brave-Browser/Default/History-journal",
    "Library/Application Support/BraveSoftware/Brave-Browser/Default/Cookies",
    "Library/Application Support/BraveSoftware/Brave-Browser/Default/Cookies-journal",
];

/// 这些文件属于哪个浏览器（进程名），清理前要它退出。
pub fn browser_of(path: &Path) -> &'static str {
    let p = path.to_string_lossy();
    if p.contains("/Google/Chrome/") {
        "Google Chrome"
    } else if p.contains("/Microsoft Edge/") {
        "Microsoft Edge"
    } else if p.contains("/Brave-Browser/") {
        "Brave Browser"
    } else {
        "Safari"
    }
}

pub const IOS_FIRMWARE_DIRS: [&str; 2] = ["Library/iTunes/iPhone Software Updates", "Library/iTunes/iPad Software Updates"];

/// Homebrew 的安装位置（Apple 芯片、Intel），用第一个装了的。
const BREW_PREFIXES: [&str; 2] = ["/opt/homebrew", "/usr/local"];
/// `brew cleanup -n` 要跑好几秒，最多等这么久。
const BREW_BUDGET: Duration = Duration::from_secs(20);

fn brew_cellar() -> PathBuf {
    let prefix = BREW_PREFIXES.iter().map(Path::new).find(|p| p.join("bin/brew").is_file()).unwrap_or(Path::new(BREW_PREFIXES[0]));
    prefix.join("Cellar")
}

/// 从 `brew cleanup -n` 的输出里取出「Would remove: <路径> (…)」那些行，只留 Cellar 下「包名/版本」这一层的目录。
/// 下载缓存那些行不要：它们在 `~/Library/Caches/Homebrew`，归「应用缓存」管。
pub(crate) fn parse_brew_cleanup(out: &str, cellar: &Path) -> Vec<PathBuf> {
    out.lines()
        .filter_map(|l| l.strip_prefix("Would remove: "))
        .map(|l| PathBuf::from(l.rsplit_once(" (").map_or(l, |(p, _)| p)))
        .filter(|p| p.strip_prefix(cellar).is_ok_and(|r| r.components().count() == 2 && r.components().all(|c| matches!(c, Component::Normal(_)))))
        .collect()
}

/// 找重复文件的位置（相对用户主目录）。
pub const DUPLICATE_DIRS: [&str; 3] = ["Downloads", "Desktop", "Documents"];
/// 小于这个大小的文件不找重复。
const DUPLICATE_MIN_BYTES: u64 = 1_000_000;
/// 「大文件和旧文件」看的位置（相对用户主目录）。音乐、图片多由各自的应用管理，不看。
pub const LARGE_FILE_DIRS: [&str; 4] = ["Desktop", "Documents", "Downloads", "Movies"];
/// 这么大的文件不管多久没打开都列出来。
const LARGE_FILE_BYTES: u64 = 50_000_000;
/// 超过这么多天没打开、又不小于 `OLD_FILE_BYTES` 的文件也列出来。
const OLD_FILE_DAYS: u64 = 365;
const OLD_FILE_BYTES: u64 = 10_000_000;
/// 最多列这么多个，按大小留最大的。
const LARGE_FILE_MAX: usize = 500;
/// 在个人文件夹里找重复文件、大文件，各自最多这么久，没找完就标「至少」。
const PERSONAL_BUDGET: Duration = Duration::from_secs(20);
/// 这些扩展名的文件夹是「包」（应用、图库、文稿包等），里面的文件不能单独删，不进去。
const PACKAGE_EXTS: [&str; 16] = ["app", "photoslibrary", "photolibrary", "musiclibrary", "tvlibrary", "imovielibrary", "fcpbundle", "logicx", "band", "bundle", "framework", "plugin", "xcarchive", "sparsebundle", "pkg", "rtfd"];
/// 文件还在 iCloud 上、没下载到本机（`SF_DATALESS`）。读它的内容会触发下载。
const SF_DATALESS: u32 = 0x4000_0000;

/// 读目录、读文件都会把它的「上次访问时间」改成现在，而旧下载、旧文件靠这个时间判断多久没用，所以读完改回原来的。
fn keep_atime(file: &fs::File, meta: &fs::Metadata) {
    if let Ok(at) = meta.accessed() {
        let _ = file.set_times(fs::FileTimes::new().set_accessed(at));
    }
}

/// 在主目录下 `dirs` 这几个位置里逐个看普通文件，交给 `f`。不跟随符号链接，不跨磁盘，
/// 跳过隐藏的、依赖目录、「包」，以及还在 iCloud 上没下载到本机的文件。返回 false 表示时限内没看完。
fn walk_personal(home: &Path, dirs: &[&str], stop: &Stop, f: &mut dyn FnMut(PathBuf, &fs::Metadata)) -> bool {
    let Ok(dev) = home.symlink_metadata().map(|m| m.dev()) else { return true };
    let mut stack: Vec<PathBuf> = dirs.iter().map(|d| home.join(d)).collect();
    while let Some(dir) = stack.pop() {
        if stop.hit() {
            return false;
        }
        let Ok(dir_meta) = dir.symlink_metadata() else { continue };
        if !dir_meta.is_dir() || dir_meta.dev() != dev {
            continue;
        }
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || PROJECT_DEP_DIRS.iter().any(|(d, _)| *d == name) {
                continue;
            }
            let path = e.path();
            let Ok(meta) = path.symlink_metadata() else { continue };
            if meta.is_dir() {
                let package = Path::new(&name).extension().and_then(|x| x.to_str()).is_some_and(|x| PACKAGE_EXTS.contains(&x.to_ascii_lowercase().as_str()));
                if !package {
                    stack.push(path);
                }
            } else if meta.is_file() && meta.dev() == dev && meta.st_flags() & SF_DATALESS == 0 {
                f(path, &meta);
            }
        }
        if let Ok(file) = fs::File::open(&dir) {
            keep_atime(&file, &dir_meta);
        }
    }
    true
}

fn last_used(meta: &fs::Metadata) -> Option<SystemTime> {
    [meta.modified(), meta.accessed()].into_iter().flatten().max()
}

/// 个人文件夹里的大文件，以及很久没打开、又不算小的文件。返回（路径，占用，上次使用），按大小降序。
fn find_large_files(home: &Path, now: SystemTime, stop: &Stop) -> (Vec<(PathBuf, u64, Option<SystemTime>)>, bool) {
    let mut found = Vec::new();
    let done = walk_personal(home, &LARGE_FILE_DIRS, stop, &mut |path, meta| {
        let size = meta.blocks() * 512;
        let last = last_used(meta);
        let old = last.and_then(|t| now.duration_since(t).ok()).is_some_and(|idle| idle >= Duration::from_secs(OLD_FILE_DAYS * 86400));
        if size >= LARGE_FILE_BYTES || (old && size >= OLD_FILE_BYTES) {
            found.push((path, size, last));
        }
    });
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    found.truncate(LARGE_FILE_MAX);
    (found, done)
}

/// 文件内容的哈希；`limit` 不为空时只看开头这么多字节。读不了或到了时限返回空。读完把访问时间改回去。
fn content_hash(path: &Path, meta: &fs::Metadata, limit: Option<u64>, stop: &Stop) -> Option<u64> {
    use std::hash::Hasher;
    use std::io::Read;
    let mut file = fs::File::open(path).ok()?;
    let mut h = std::hash::DefaultHasher::new();
    let mut buf = vec![0u8; limit.unwrap_or(1 << 20).min(1 << 20) as usize];
    let mut left = limit.unwrap_or(u64::MAX);
    let mut ok = true;
    while left > 0 {
        if stop.hit() {
            ok = false;
            break;
        }
        let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        match file.read(&mut buf[..want]) {
            Ok(0) => break,
            Ok(n) => {
                h.write(&buf[..n]);
                left -= n as u64;
            }
            Err(_) => {
                ok = false;
                break;
            }
        }
    }
    keep_atime(&file, meta);
    ok.then(|| h.finish())
}

/// 把一组文件按 `key` 再分组，只留至少两个的组；`key` 为空的文件丢掉。
fn split_by(group: Vec<(PathBuf, fs::Metadata)>, key: impl Fn(&Path, &fs::Metadata) -> Option<u64>) -> Vec<Vec<(PathBuf, fs::Metadata)>> {
    let mut by: HashMap<u64, Vec<(PathBuf, fs::Metadata)>> = HashMap::new();
    for (p, m) in group {
        if let Some(k) = key(&p, &m) {
            by.entry(k).or_default().push((p, m));
        }
    }
    by.into_values().filter(|g| g.len() > 1).collect()
}

/// 下载、桌面、文稿里内容完全相同的文件：先按大小分组，再比开头 64 KB 的哈希，最后比整个文件的哈希。
/// 每组留一份（优先留不在「下载」里的，再留最早的），其余返回（路径，占用，上次使用）。
fn find_duplicates(home: &Path, stop: &Stop) -> (Vec<(PathBuf, u64, Option<SystemTime>)>, bool) {
    let mut by_len: HashMap<u64, Vec<(PathBuf, fs::Metadata)>> = HashMap::new();
    let mut done = walk_personal(home, &DUPLICATE_DIRS, stop, &mut |path, meta| {
        // 硬链接本来就是同一份数据，删了也不释放空间
        if meta.len() >= DUPLICATE_MIN_BYTES && meta.nlink() == 1 {
            by_len.entry(meta.len()).or_default().push((path, meta.clone()));
        }
    });
    let downloads = home.join("Downloads");
    let mut found = Vec::new();
    for group in by_len.into_values().filter(|g| g.len() > 1) {
        for head in split_by(group, |p, m| content_hash(p, m, Some(64 * 1024), stop)) {
            for mut same in split_by(head, |p, m| content_hash(p, m, None, stop)) {
                same.sort_by_key(|(p, m)| (p.starts_with(&downloads), m.created().or_else(|_| m.modified()).ok(), p.clone()));
                found.extend(same.into_iter().skip(1).map(|(p, m)| (p, m.blocks() * 512, last_used(&m))));
            }
        }
        if stop.hit() {
            done = false;
            break;
        }
    }
    (found, done)
}

/// 项目里可以重新生成的依赖和构建目录：目录名 → 证明它确实是这类产物的标志文件（相对项目目录，有一个就算）。
pub const PROJECT_DEP_DIRS: [(&str, &[&str]); 6] = [
    ("node_modules", &["package.json"]),
    ("target", &["Cargo.toml", "pom.xml"]),
    ("Pods", &["Podfile"]),
    ("build", &["build.gradle", "build.gradle.kts"]),
    (".venv", &[".venv/pyvenv.cfg"]),
    ("venv", &["venv/pyvenv.cfg"]),
];
/// 项目里所有文件都超过这么多天没改过，才算「不活跃」。
pub const STALE_PROJECT_DAYS: u64 = 90;
/// 主目录下不进去找的文件夹：前几个里不会有项目；「下载」整个归「旧下载文件」那一类管，免得同一处算两遍。
const STALE_SKIP_TOP: [&str; 7] = ["Library", "Applications", "Pictures", "Music", "Movies", "Public", "Downloads"];
/// 小于这个大小的依赖目录不值得列出来。
const STALE_MIN_BYTES: u64 = 1 << 20;
/// 从主目录往下最多找这么多层。
const STALE_MAX_DEPTH: u32 = 6;
/// 判断一个项目活不活跃时最多看这么多个文件；超过就当作「说不准」，不列出。
const STALE_ACTIVITY_FILES: u32 = 3000;
/// 找项目和量大小各自的时限，超时就标「至少」。
const STALE_FIND_BUDGET: Duration = Duration::from_secs(15);
const STALE_SIZE_BUDGET: Duration = Duration::from_secs(20);

/// 项目最后一次有文件被改动的时间。看项目里三层以内的文件（不进依赖目录），
/// 再加上每个依赖目录里第一层的文件：构建和安装依赖会改到它们，只运行不改代码的项目也算活跃。
/// 文件太多看不完、或一个文件都没有时返回空，调用方把它当作「说不准」。
fn last_activity(project: &Path, deps: &[&str]) -> Option<SystemTime> {
    let mut latest: Option<SystemTime> = None;
    let mut seen = 0u32;
    let mut stack: Vec<(PathBuf, u32)> = vec![(project.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        for e in fs::read_dir(&dir).ok()?.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let name = e.file_name();
            if ft.is_file() {
                if name == ".DS_Store" {
                    continue;
                }
                seen += 1;
                if seen > STALE_ACTIVITY_FILES {
                    return None;
                }
                latest = latest.max(e.metadata().ok().and_then(|m| m.modified().ok()));
            } else if ft.is_dir() {
                let is_dep = PROJECT_DEP_DIRS.iter().any(|(d, _)| name == *d);
                // 依赖目录只在它确实属于这个项目时看一层；别的目录最多往下三层
                if (depth == 0 && deps.iter().any(|d| name == *d)) || (!is_dep && depth < 3) {
                    stack.push((e.path(), if is_dep { 3 } else { depth + 1 }));
                }
            }
        }
    }
    latest
}

/// 在主目录里找不活跃项目留下的依赖目录。只读，不跟随符号链接。
/// 返回（依赖目录，项目最后改动时间）；第二个值为 false 表示时限内没找完。
pub(crate) fn find_stale_deps(home: &Path, now: SystemTime, days: u64, stop: &Stop) -> (Vec<(PathBuf, SystemTime)>, bool) {
    let mut found = Vec::new();
    let mut stack: Vec<(PathBuf, u32)> = vec![(home.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if stop.hit() {
            return (found, false);
        }
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        let (mut subdirs, mut files) = (Vec::new(), HashSet::new());
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            match e.file_type() {
                Ok(ft) if ft.is_dir() => subdirs.push(name),
                Ok(ft) if ft.is_file() => drop(files.insert(name)),
                _ => {}
            }
        }
        let deps: Vec<&str> = PROJECT_DEP_DIRS
            .iter()
            .filter(|(d, markers)| subdirs.iter().any(|s| s == d) && markers.iter().any(|m| if m.contains('/') { dir.join(m).is_file() } else { files.contains(*m) }))
            .map(|(d, _)| *d)
            .collect();
        if !deps.is_empty() {
            if let Some(last) = last_activity(&dir, &deps) {
                if now.duration_since(last).is_ok_and(|idle| idle >= Duration::from_secs(days * 86400)) {
                    found.extend(deps.iter().map(|d| (dir.join(d), last)));
                }
            }
        }
        if depth < STALE_MAX_DEPTH {
            for s in subdirs {
                let skip = s.starts_with('.') || s.ends_with(".app") || PROJECT_DEP_DIRS.iter().any(|(d, _)| *d == s) || (depth == 0 && STALE_SKIP_TOP.contains(&s.as_str()));
                if !skip {
                    stack.push((dir.join(s), depth + 1));
                }
            }
        }
    }
    (found, true)
}

impl CategoryId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AppCache => "app_cache",
            Self::SystemCache => "system_cache",
            Self::Trash => "trash",
            Self::OldDownloads => "old_downloads",
            Self::Logs => "logs",
            Self::DevCache => "dev_cache",
            Self::StaleProjects => "stale_projects",
            Self::AiCache => "ai_cache",
            Self::AiHistory => "ai_history",
            Self::CrashDumps => "crash_dumps",
            Self::IosBackups => "ios_backups",
            Self::XcodeArchives => "xcode_archives",
            Self::IosFirmware => "ios_firmware",
            Self::MailDownloads => "mail_downloads",
            Self::HomebrewOld => "homebrew_old",
            Self::Duplicates => "duplicates",
            Self::LargeFiles => "large_files",
            Self::BrowserPrivacy => "browser_privacy",
        }
    }
}

/// 首版的六类规则，一类一条。
pub fn default_rules(home: &Path) -> Vec<Rule> {
    let rule = |id, root: PathBuf| Rule {
        id,
        root,
        default_selected: true,
        needs_admin: false,
        needs_full_disk_access: false,
        older_than_days: None,
        skip_prefixes: &[],
        only: &[],
    };
    vec![
        // 系统自带组件的缓存由系统自己管理，删了可能让系统功能短暂异常，所以不列出；本应用自己的也不列；
        // Claude Code 的日志归「AI 工具缓存」管（工具可能正开着，默认不勾选），这里不重复列
        Rule { skip_prefixes: &["com.apple.", "com.pikaclean.", "claude-cli-nodejs"], ..rule(CategoryId::AppCache, home.join("Library/Caches")) },
        // 清理它要输管理员密码，所以默认不勾选，免得每次清理都弹密码框
        Rule { needs_admin: true, default_selected: false, ..rule(CategoryId::SystemCache, PathBuf::from("/Library/Caches")) },
        Rule { needs_full_disk_access: true, ..rule(CategoryId::Trash, home.join(".Trash")) },
        Rule { default_selected: false, older_than_days: Some(OLD_DOWNLOAD_DAYS), ..rule(CategoryId::OldDownloads, home.join("Downloads")) },
        rule(CategoryId::Logs, home.join("Library/Logs")),
        // 删了以后下次构建要重新下载依赖，比较费时间，所以默认不勾选
        Rule { default_selected: false, only: &DEV_CACHE_DIRS, ..rule(CategoryId::DevCache, home.to_path_buf()) },
        // 动的是用户项目目录里的东西，所以默认不勾选，而且只移到废纸篓
        Rule { default_selected: false, older_than_days: Some(STALE_PROJECT_DAYS), ..rule(CategoryId::StaleProjects, home.to_path_buf()) },
        // 工具可能正开着，删缓存会打断它，所以默认不勾选
        Rule { default_selected: false, only: &AI_CACHE_DIRS, ..rule(CategoryId::AiCache, home.to_path_buf()) },
        // 对话记录是用户自己的东西：默认不勾选，只移到废纸篓
        Rule { default_selected: false, older_than_days: Some(AI_HISTORY_DAYS), ..rule(CategoryId::AiHistory, home.to_path_buf()) },
        // 文件属主是 root，删除要输管理员密码；只在排查崩溃时有用，默认不勾选
        Rule { needs_admin: true, default_selected: false, ..rule(CategoryId::CrashDumps, PathBuf::from(crate::admin::CORES_DIR)) },
        // 设备备份往往是那台设备数据仅有的一份：默认不勾选，只移到废纸篓
        Rule { default_selected: false, needs_full_disk_access: true, ..rule(CategoryId::IosBackups, home.join("Library/Application Support/MobileSync/Backup")) },
        // 归档里有还原崩溃日志要用的符号文件，删了就找不回：默认不勾选，只移到废纸篓
        Rule { default_selected: false, ..rule(CategoryId::XcodeArchives, home.join("Library/Developer/Xcode/Archives")) },
        // 固件可以重新下载，但一个就有好几 GB，所以默认不勾选
        Rule { default_selected: false, only: &IOS_FIRMWARE_DIRS, ..rule(CategoryId::IosFirmware, home.to_path_buf()) },
        // 附件原件还在邮件里，但打开后改过的副本可能只在这里：默认不勾选，只移到废纸篓
        Rule { default_selected: false, needs_full_disk_access: true, ..rule(CategoryId::MailDownloads, home.join("Library/Containers/com.apple.mail/Data/Library/Mail Downloads")) },
        // 只列 brew 自己认为可以删的旧版本；默认不勾选
        Rule { default_selected: false, ..rule(CategoryId::HomebrewOld, brew_cellar()) },
        // 删了会退出各网站的登录：默认不勾选，只进废纸篓。Safari 的记录要完全磁盘访问才读得到，读不到的文件不列
        Rule { default_selected: false, only: &BROWSER_PRIVACY_FILES, ..rule(CategoryId::BrowserPrivacy, home.to_path_buf()) },
        // 下面两类是个人文件：默认不勾选，只移到废纸篓。放在最后，前面各类已经列过的文件不再重复列
        Rule { default_selected: false, ..rule(CategoryId::Duplicates, home.to_path_buf()) },
        Rule { default_selected: false, ..rule(CategoryId::LargeFiles, home.to_path_buf()) },
    ]
}

/// 实际占用的磁盘空间，到了 `deadline` 就停。返回（已经数到的大小，是否数完了）。
/// 不跟随符号链接，不跨出 `dev` 所在的磁盘。有的目录里有上百万个小文件，不设时限的话界面会一直等。
pub(crate) fn disk_usage_until(path: &Path, dev: u64, deadline: Option<std::time::Instant>) -> (u64, bool) {
    static NEVER: AtomicBool = AtomicBool::new(false);
    measure(path, dev, deadline.map(|deadline| Stop { deadline, cancel: &NEVER }).as_ref())
}

fn measure(path: &Path, dev: u64, stop: Option<&Stop>) -> (u64, bool) {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    let mut seen = 0u32;
    // 链接数大于 1 的文件按 inode 只算一次（和空间去向同口径）
    let mut linked = HashSet::new();
    while let Some(p) = stack.pop() {
        seen += 1;
        // 看时间本身也有开销，每数一批文件看一次
        if seen % 512 == 0 && stop.is_some_and(Stop::hit) {
            return (total, false);
        }
        let Ok(meta) = p.symlink_metadata() else { continue };
        if meta.dev() != dev {
            continue;
        }
        if !meta.is_dir() && meta.nlink() > 1 && !linked.insert(meta.ino()) {
            continue;
        }
        total += meta.blocks() * 512;
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&p) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
        }
    }
    (total, true)
}

fn millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 向系统的搜索索引问：`root` 下最近 `days` 天内被打开过或新加进来的一级条目有哪些。
/// 文件自身记录的时间不可靠（解压出来的旧文件、从没打开过的新下载），所以两边都看。
pub fn spotlight_recent(root: &Path, days: u64) -> HashSet<PathBuf> {
    let query = format!("kMDItemLastUsedDate >= $time.today(-{days}) || kMDItemDateAdded >= $time.today(-{days})");
    // 索引正在重建时可能很久不回，最多等几秒，等不到就当作没有
    let Ok(out) = crate::devenv::run(Path::new("/usr/bin/mdfind"), &["-onlyin", &root.to_string_lossy(), &query], Duration::from_secs(5)) else {
        return HashSet::new();
    };
    out.lines()
        .filter_map(|l| Path::new(l).strip_prefix(root).ok()?.components().next().map(|c| root.join(c)))
        .collect()
}

fn scan_category(rule: &Rule, now: SystemTime, recent: &dyn Fn(&Path, u64) -> HashSet<PathBuf>, stop: &Stop) -> Category {
    let mut cat = Category {
        id: rule.id,
        root: rule.root.clone(),
        total_bytes: 0,
        items: Vec::new(),
        default_selected: rule.default_selected,
        removal: crate::remove::mode_of(rule.id),
        needs_admin: rule.needs_admin,
        needs_full_disk_access: rule.needs_full_disk_access,
        unreadable: false,
        partial: false,
        only: rule.only.iter().map(|s| (*s).to_owned()).collect(),
    };
    if matches!(rule.id, CategoryId::Duplicates | CategoryId::LargeFiles) {
        // 个人文件夹里找出来的单个文件，大小直接取文件本身的占用；找到的路径记进 only，删除前的守卫只认这几个
        let personal = stop.within(PERSONAL_BUDGET);
        let (found, done) = if rule.id == CategoryId::Duplicates { find_duplicates(&rule.root, &personal) } else { find_large_files(&rule.root, now, &personal) };
        cat.partial = !done;
        for (path, size_bytes, last) in found {
            let Ok(rel) = path.strip_prefix(&rule.root) else { continue };
            cat.only.push(rel.to_string_lossy().into_owned());
            cat.items.push(Item { id: format!("{}:{}", rule.id.as_str(), path.display()), size_bytes, last_used_ms: last.map(millis), path, partial: false });
        }
        cat.items.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path)));
        cat.total_bytes = cat.items.iter().map(|i| i.size_bytes).sum();
        return cat;
    }
    if matches!(rule.id, CategoryId::StaleProjects | CategoryId::AiHistory | CategoryId::HomebrewOld) {
        // 这几类的条目是找出来的，不是写死的；找到的路径记进 only，删除前的守卫照样只认这几个
        let found: Vec<(PathBuf, SystemTime)> = if rule.id == CategoryId::AiHistory {
            let found = find_ai_history(&rule.root, now, rule.older_than_days.unwrap_or(AI_HISTORY_DAYS));
            found.into_iter().filter_map(|p| Some((p.clone(), p.symlink_metadata().ok()?.modified().ok()?))).collect()
        } else if rule.id == CategoryId::HomebrewOld {
            // 只问 brew 自己：被固定的、还有别的包在用的旧版本，它不会列出来
            let brew = rule.root.with_file_name("bin/brew");
            let timeout = stop.within(BREW_BUDGET).deadline.saturating_duration_since(Instant::now());
            let out = if brew.is_file() { crate::devenv::run(&brew, &["cleanup", "-n"], timeout).unwrap_or_default() } else { String::new() };
            parse_brew_cleanup(&out, &rule.root).into_iter().filter_map(|p| Some((p.clone(), p.symlink_metadata().ok()?.modified().ok()?))).collect()
        } else {
            let days = rule.older_than_days.unwrap_or(STALE_PROJECT_DAYS);
            let (found, done) = find_stale_deps(&rule.root, now, days, &stop.within(STALE_FIND_BUDGET));
            cat.partial = !done;
            found
        };
        let size_stop = stop.within(STALE_SIZE_BUDGET);
        for (path, last) in found {
            let (Ok(meta), Ok(rel)) = (path.symlink_metadata(), path.strip_prefix(&rule.root)) else { continue };
            let (size_bytes, done) = measure(&path, meta.dev(), Some(&size_stop));
            cat.partial |= !done;
            if rule.id == CategoryId::StaleProjects && done && size_bytes < STALE_MIN_BYTES {
                continue;
            }
            cat.only.push(rel.to_string_lossy().into_owned());
            cat.items.push(Item { id: format!("{}:{}", rule.id.as_str(), path.display()), size_bytes, last_used_ms: Some(millis(last)), path, partial: !done });
        }
        cat.items.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path)));
        cat.total_bytes = cat.items.iter().map(|i| i.size_bytes).sum();
        return cat;
    }
    if !rule.only.is_empty() {
        // 固定路径的类别：只看规则里写明的那几个目录（浏览器隐私是文件），存在且不是符号链接才算
        for rel in rule.only {
            let path = rule.root.join(rel);
            let Ok(meta) = path.symlink_metadata() else { continue };
            if !(meta.is_dir() || rule.id == CategoryId::BrowserPrivacy && meta.is_file()) {
                continue;
            }
            let (size_bytes, done) = measure(&path, meta.dev(), Some(&stop.within(ONLY_ITEM_BUDGET)));
            cat.partial |= !done;
            cat.items.push(Item {
                id: format!("{}:{}", rule.id.as_str(), path.display()),
                size_bytes,
                last_used_ms: [meta.modified(), meta.accessed()].into_iter().flatten().max().map(millis),
                path,
                partial: !done,
            });
        }
        cat.items.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path)));
        cat.total_bytes = cat.items.iter().map(|i| i.size_bytes).sum();
        return cat;
    }
    let entries = match fs::read_dir(&rule.root) {
        Ok(e) => e,
        Err(e) if e.kind() == ErrorKind::NotFound => return cat,
        Err(_) => {
            cat.unreadable = true;
            return cat;
        }
    };
    let dev = fs::metadata(&rule.root).map(|m| m.dev()).unwrap_or(0);
    let recent_set = rule.older_than_days.map(|d| recent(&rule.root, d)).unwrap_or_default();
    let size_stop = stop.within(LIST_SIZE_BUDGET);

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if SKIP_NAMES.contains(&name.as_str()) || rule.skip_prefixes.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = path.symlink_metadata() else { continue };
        if !rule.needs_admin && locked_by_system(&path, &meta) {
            continue;
        }
        // 崩溃转储只认 core.<数字> 普通文件；内容 root 才能读，但大小能直接看
        if rule.id == CategoryId::CrashDumps && !(meta.is_file() && crate::admin::is_core_dump_name(&name)) {
            continue;
        }
        let last_used = [meta.modified(), meta.accessed()].into_iter().flatten().max();
        if let Some(days) = rule.older_than_days {
            let idle = last_used.and_then(|t| now.duration_since(t).ok()).unwrap_or(Duration::ZERO);
            if recent_set.contains(&path) || idle < Duration::from_secs(days * 86400) {
                continue;
            }
        }
        let (size_bytes, done) = measure(&path, dev, Some(&size_stop));
        cat.partial |= !done;
        cat.items.push(Item { id: format!("{}:{}", rule.id.as_str(), path.display()), size_bytes, last_used_ms: last_used.map(millis), path, partial: !done });
    }
    cat.items.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.path.cmp(&b.path)));
    cat.total_bytes = cat.items.iter().map(|i| i.size_bytes).sum();
    cat
}

/// 系统保护的文件夹（如 `~/Library/Caches/CloudKit`，受系统完整性保护）文件夹本身属于用户，但连读都不让读，也删不掉，不列出来。
/// 要管理员权限的类别不用它：那些文件夹本来就是普通用户读不了、交给密码框去删的。
fn locked_by_system(path: &Path, meta: &fs::Metadata) -> bool {
    meta.is_dir() && fs::read_dir(path).is_err_and(|e| e.kind() == ErrorKind::PermissionDenied)
}

/// 按规则扫描，几类同时扫，结果仍按规则的顺序。`recent` 用来查「最近用过的条目」，正式运行时传 [`spotlight_recent`]，测试时传假的。
/// `cancel` 被置上后各类马上停手，返回的是不完整的报告，调用方应丢掉它。
pub fn scan(
    rules: &[Rule],
    now: SystemTime,
    recent: &(dyn Fn(&Path, u64) -> HashSet<PathBuf> + Sync),
    on_progress: &(dyn Fn(Progress) + Sync),
    cancel: &AtomicBool,
) -> ScanReport {
    let stop = Stop { deadline: Instant::now() + SCAN_BUDGET, cancel };
    let total = rules.len() as u32;
    // 每开始一类发一次进度，done 是已经扫完的类别数
    let (next, done) = (AtomicUsize::new(0), AtomicU32::new(0));
    let mut out: Vec<(usize, Category)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..SCAN_THREADS)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(rule) = rules.get(i) else { break mine };
                        on_progress(Progress { category: rule.id, done: done.load(Ordering::Relaxed), total });
                        mine.push((i, scan_category(rule, now, recent, &stop)));
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    out.sort_by_key(|(i, _)| *i);
    let mut categories: Vec<Category> = out.into_iter().map(|(_, c)| c).collect();
    drop_covered(&mut categories);
    let at_ms = now.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    ScanReport { categories, at_ms }
}

/// 同一个文件只算在一类里：路径（或它所在的文件夹）已经列在前面某一类里的，后面的类别不再列。
/// 不然合计会多算一份，两类都勾时后清的那一项还会报「条目已不存在」。
fn drop_covered(categories: &mut [Category]) {
    let mut listed: HashSet<PathBuf> = HashSet::new();
    for c in categories {
        c.items.retain(|i| !i.path.ancestors().any(|a| listed.contains(a)));
        c.total_bytes = c.items.iter().map(|i| i.size_bytes).sum();
        listed.extend(c.items.iter().map(|i| i.path.clone()));
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 每个测试一棵独立的临时目录树，离开作用域时自动删除。
    pub struct TempTree(pub PathBuf);
    impl TempTree {
        pub fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!("pika-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).unwrap();
            // 临时目录本身常常是符号链接（/var → /private/var），统一用解析后的路径
            Self(dir.canonicalize().unwrap())
        }
        pub fn file(&self, rel: &str, bytes: usize) -> PathBuf {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![7u8; bytes]).unwrap();
            p
        }
    }
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::TempTree;
    use super::*;

    fn rule(id: CategoryId, root: PathBuf) -> Rule {
        Rule { id, root, default_selected: true, needs_admin: false, needs_full_disk_access: false, older_than_days: None, skip_prefixes: &[], only: &[] }
    }
    fn none(_: &Path, _: u64) -> HashSet<PathBuf> {
        HashSet::new()
    }
    fn run(rules: &[Rule]) -> ScanReport {
        scan(rules, SystemTime::now(), &none, &|_| {}, &AtomicBool::new(false))
    }

    #[test]
    fn 崩溃转储只列出_core_数字_普通文件() {
        let t = TempTree::new();
        let ok = t.file("cores/core.42", 4096);
        t.file("cores/core.x", 10);
        t.file("cores/core.9/inside", 10);
        std::os::unix::fs::symlink(t.file("elsewhere", 10), t.0.join("cores/core.7")).unwrap();
        let r = run(&[rule(CategoryId::CrashDumps, t.0.join("cores"))]);
        let items: Vec<_> = r.categories[0].items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(items, vec![ok]);
        assert!(r.categories[0].total_bytes >= 4096);
    }

    #[test]
    fn 条目数和大小与目录内容一致() {
        let t = TempTree::new();
        t.file("caches/a/big.bin", 300_000);
        t.file("caches/a/sub/more.bin", 100_000);
        t.file("caches/b.bin", 50_000);
        t.file("caches/.DS_Store", 10);
        let r = run(&[rule(CategoryId::AppCache, t.0.join("caches"))]);
        let c = &r.categories[0];
        assert_eq!(c.items.len(), 2, ".DS_Store 不算条目");
        assert!(c.items[0].path.ends_with("a") && c.items[0].size_bytes >= 400_000, "按大小降序，目录大小含子目录");
        assert!(c.items[1].size_bytes >= 50_000 && c.items[1].size_bytes < 100_000);
        assert_eq!(c.total_bytes, c.items.iter().map(|i| i.size_bytes).sum::<u64>());
        assert_eq!(c.items[0].id, format!("app_cache:{}", c.items[0].path.display()));
    }

    #[test]
    fn 符号链接指向的外部文件不计入大小() {
        let t = TempTree::new();
        let outside = t.file("outside/huge.bin", 2_000_000);
        t.file("caches/a/small.bin", 1_000);
        std::os::unix::fs::symlink(&outside, t.0.join("caches/a/link")).unwrap();
        let r = run(&[rule(CategoryId::AppCache, t.0.join("caches"))]);
        assert!(r.categories[0].total_bytes < 100_000);
    }

    #[test]
    fn 旧下载只列超过九十天没用过的() {
        let t = TempTree::new();
        let day = Duration::from_secs(86400);
        let age = |p: &Path, days: u32| {
            let when = SystemTime::now() - day * days;
            let f = fs::File::options().write(true).open(p).unwrap();
            f.set_times(fs::FileTimes::new().set_accessed(when).set_modified(when)).unwrap();
        };
        age(&t.file("dl/old.zip", 100), 91);
        age(&t.file("dl/fresh.zip", 100), 89);
        let opened_recently = t.file("dl/old-but-opened.pdf", 100);
        age(&opened_recently, 200);
        let r = Rule { older_than_days: Some(90), ..rule(CategoryId::OldDownloads, t.0.join("dl")) };
        let recent = |_: &Path, _: u64| HashSet::from([opened_recently.clone()]);
        let report = scan(&[r], SystemTime::now(), &recent, &|_| {}, &AtomicBool::new(false));
        let names: Vec<_> = report.categories[0].items.iter().map(|i| i.path.file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["old.zip"]);
    }

    #[test]
    fn 位置不存在时为空且不报错() {
        let t = TempTree::new();
        let r = run(&[rule(CategoryId::Logs, t.0.join("nope"))]);
        assert!(!r.categories[0].unreadable && r.categories[0].items.is_empty() && r.categories[0].total_bytes == 0);
    }

    #[test]
    fn 没有读取权限时标记为读不到且不影响其他类别() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempTree::new();
        t.file("locked/x.bin", 10);
        t.file("logs/y.log", 10);
        fs::set_permissions(t.0.join("locked"), fs::Permissions::from_mode(0o000)).unwrap();
        let r = run(&[rule(CategoryId::Trash, t.0.join("locked")), rule(CategoryId::Logs, t.0.join("logs"))]);
        fs::set_permissions(t.0.join("locked"), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(r.categories[0].unreadable && r.categories[0].items.is_empty());
        assert_eq!(r.categories[1].items.len(), 1);
    }

    #[test]
    fn 读不了的子文件夹不列出来() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempTree::new();
        t.file("caches/CloudKit/x.db", 10);
        t.file("caches/com.example.app/y.cache", 10);
        fs::set_permissions(t.0.join("caches/CloudKit"), fs::Permissions::from_mode(0o000)).unwrap();
        let r = run(&[rule(CategoryId::AppCache, t.0.join("caches"))]);
        fs::set_permissions(t.0.join("caches/CloudKit"), fs::Permissions::from_mode(0o755)).unwrap();
        let names: Vec<_> = r.categories[0].items.iter().map(|i| i.path.file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["com.example.app"]);
        assert!(!r.categories[0].unreadable);
    }

    /// 只读探针，平时不跑。在真实的用户目录上扫一遍，打印每一类找到多少、花了多久，不删任何东西：
    /// cargo test --release --lib 在本机真实目录上扫一遍 -- --ignored --nocapture
    #[test]
    #[ignore]
    fn 在本机真实目录上扫一遍() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let started = std::time::Instant::now();
        let report = scan(&default_rules(&home), SystemTime::now(), &spotlight_recent, &|_| {}, &AtomicBool::new(false));
        for c in &report.categories {
            println!("{:<15} {:>5} 项 {:>9.1} MB{}{}", c.id.as_str(), c.items.len(), c.total_bytes as f64 / 1_048_576.0, if c.partial { "（至少）" } else { "" }, if c.unreadable { "（读不到）" } else { "" });
            if c.id == CategoryId::StaleProjects {
                for i in &c.items {
                    println!("  {:>9.1} MB  {}", i.size_bytes as f64 / 1_048_576.0, i.path.strip_prefix(&home).unwrap_or(&i.path).display());
                }
            }
        }
        println!("用时 {:.1} 秒", started.elapsed().as_secs_f64());
    }

    #[test]
    fn 只列出不活跃项目里有标志文件作证的依赖目录() {
        let t = TempTree::new();
        let old = SystemTime::now() - Duration::from_secs(200 * 86400);
        let age = |rel: &str| fs::File::options().write(true).open(t.0.join(rel)).unwrap().set_modified(old).unwrap();
        // 很久没动的前端项目：列出
        for f in ["code/old-web/package.json", "code/old-web/src/a.js", "code/old-web/node_modules/x/index.js"] {
            t.file(f, 2_000_000);
            age(f);
        }
        // 一样很久没动，但依赖目录小到不值得列
        for f in ["code/tiny/package.json", "code/tiny/node_modules/x"] {
            t.file(f, 10);
            age(f);
        }
        // 「下载」里的归别的类别管
        for f in ["Downloads/dl/package.json", "Downloads/dl/node_modules/x"] {
            t.file(f, 2_000_000);
            age(f);
        }
        // 代码没改、但最近装过依赖：算活跃
        for f in ["code/running/package.json", "code/running/node_modules/x/i.js"] {
            t.file(f, 10);
            age(f);
        }
        t.file("code/running/node_modules/.package-lock.json", 10);
        // 最近改过代码：算活跃
        t.file("code/fresh/Cargo.toml", 10);
        age("code/fresh/Cargo.toml");
        t.file("code/fresh/src/main.rs", 10);
        t.file("code/fresh/target/debug/x", 10);
        // 名字像依赖目录，但旁边没有标志文件：不碰
        for f in ["code/photos/build/keep.jpg", "code/photos/target/keep.txt"] {
            t.file(f, 10);
            age(f);
        }
        // 不进去找的地方：隐藏目录、主目录下的 Library
        for f in [".hidden/p/package.json", ".hidden/p/node_modules/x", "Library/p/package.json", "Library/p/node_modules/x"] {
            t.file(f, 10);
            age(f);
        }
        let r = Rule { default_selected: false, older_than_days: Some(STALE_PROJECT_DAYS), ..rule(CategoryId::StaleProjects, t.0.clone()) };
        let c = run(&[r]).categories.remove(0);
        let names: Vec<String> = c.items.iter().map(|i| i.path.strip_prefix(&t.0).unwrap().display().to_string()).collect();
        assert_eq!(names, ["code/old-web/node_modules"]);
        assert_eq!(c.only, ["code/old-web/node_modules"]);
        assert!(c.total_bytes >= 2_000_000 && !c.partial && !c.default_selected && c.removal == crate::remove::Mode::Trash);
    }

    #[test]
    fn 对话记录只列出很久没动的会话且不碰长期记忆() {
        let t = TempTree::new();
        let old = SystemTime::now() - Duration::from_secs(60 * 86400);
        let age = |rel: &str| fs::File::options().write(true).open(t.0.join(rel)).unwrap().set_modified(old).unwrap();
        for f in [".claude/projects/p/old.jsonl", ".claude/projects/p/memory/MEMORY.md", ".codex/archived_sessions/a.jsonl", ".codex/sessions/2026/01/02/r.jsonl"] {
            t.file(f, 10);
            age(f);
        }
        t.file(".claude/projects/p/new.jsonl", 10);
        for d in [".claude/projects/p/memory", ".codex/sessions/2026/01/02"] {
            fs::File::open(t.0.join(d)).unwrap().set_modified(old).unwrap();
        }
        let r = Rule { default_selected: false, older_than_days: Some(AI_HISTORY_DAYS), ..rule(CategoryId::AiHistory, t.0.clone()) };
        let c = run(&[r]).categories.remove(0);
        let mut names: Vec<String> = c.items.iter().map(|i| i.path.strip_prefix(&t.0).unwrap().display().to_string()).collect();
        names.sort();
        assert_eq!(names, [".claude/projects/p/old.jsonl", ".codex/archived_sessions/a.jsonl", ".codex/sessions/2026/01/02"]);
        assert_eq!(c.only.len(), 3);
        assert!(!c.default_selected && c.removal == crate::remove::Mode::Trash);
    }

    #[test]
    fn 固定路径的类别只看规则里写明的目录() {
        let t = TempTree::new();
        t.file(".npm/_cacache/a/b", 40_000);
        t.file(".gradle/caches/x", 10_000);
        t.file(".npm/other/keep", 10);
        t.file("Documents/secret", 10);
        // 写明的路径是个文件、或是个符号链接，都不算
        t.file(".cache/yarn", 10);
        std::os::unix::fs::symlink(t.0.join("Documents"), t.0.join(".m2-link")).unwrap();
        let r = Rule { only: &[".npm/_cacache", ".gradle/caches", ".cache/yarn", ".m2-link", ".nope"], default_selected: false, ..rule(CategoryId::DevCache, t.0.clone()) };
        let c = run(&[r]).categories.remove(0);
        let names: Vec<String> = c.items.iter().map(|i| i.path.strip_prefix(&t.0).unwrap().display().to_string()).collect();
        assert_eq!(names, [".npm/_cacache", ".gradle/caches"]);
        assert!(!c.partial && !c.default_selected && c.total_bytes >= 50_000);
        assert_eq!(c.only.len(), 5);
    }

    #[test]
    fn 浏览器隐私只列写明的记录和_cookie_文件() {
        let t = TempTree::new();
        let chrome = "Library/Application Support/Google/Chrome/Default";
        t.file(&format!("{chrome}/History"), 3_000);
        t.file(&format!("{chrome}/Cookies"), 1_000);
        t.file(&format!("{chrome}/Bookmarks"), 10);
        t.file(&format!("{chrome}/Login Data"), 10);
        let r = default_rules(&t.0).into_iter().find(|r| r.id == CategoryId::BrowserPrivacy).unwrap();
        let c = run(&[r]).categories.remove(0);
        let mut names: Vec<String> = c.items.iter().map(|i| i.path.strip_prefix(&t.0).unwrap().display().to_string()).collect();
        names.sort();
        assert_eq!(names, [format!("{chrome}/Cookies"), format!("{chrome}/History")], "书签和密码不碰");
        assert!(!c.default_selected && c.removal == crate::remove::Mode::Trash);
        assert_eq!(browser_of(&c.items[0].path), "Google Chrome");
    }

    #[test]
    fn 按前缀跳过并逐类报告进度() {
        let t = TempTree::new();
        t.file("caches/com.apple.Safari/x", 10);
        t.file("caches/com.other.app/x", 10);
        let seen = std::sync::Mutex::new(vec![]);
        let rules = [
            Rule { skip_prefixes: &["com.apple."], ..rule(CategoryId::AppCache, t.0.join("caches")) },
            rule(CategoryId::Logs, t.0.join("logs")),
        ];
        let r = scan(&rules, SystemTime::now(), &none, &|p| seen.lock().unwrap().push((p.category, p.done, p.total)), &AtomicBool::new(false));
        assert_eq!(r.categories[0].items.len(), 1);
        // 几类同时扫：每类开始时各发一次，先后不定；结果仍按规则的顺序
        let mut seen = seen.into_inner().unwrap();
        seen.sort_by_key(|(c, ..)| c.as_str());
        assert_eq!(seen.iter().map(|(c, _, total)| (*c, *total)).collect::<Vec<_>>(), [(CategoryId::AppCache, 2), (CategoryId::Logs, 2)]);
        assert!(seen.iter().all(|(_, done, _)| *done < 2));
        assert_eq!(r.categories.iter().map(|c| c.id).collect::<Vec<_>>(), [CategoryId::AppCache, CategoryId::Logs]);
    }

    #[test]
    fn 取消后各类马上停手() {
        let t = TempTree::new();
        for i in 0..2000 {
            t.file(&format!("caches/a/{i}"), 1);
        }
        let r = scan(&[rule(CategoryId::AppCache, t.0.join("caches"))], SystemTime::now(), &none, &|_| {}, &AtomicBool::new(true));
        let c = &r.categories[0];
        assert!(c.partial && c.items[0].partial, "没量完的条目和类别都标「至少」");
    }

    #[test]
    fn 量大小时硬链接只算一次() {
        let t = TempTree::new();
        let a = t.file("d/a", 100_000);
        fs::hard_link(&a, t.0.join("d/b")).unwrap();
        let dev = t.0.symlink_metadata().unwrap().dev();
        let (size, done) = disk_usage_until(&t.0.join("d"), dev, None);
        assert!(done && size >= 100_000 && size < 200_000, "{size}");
    }

    #[test]
    fn 固定路径不落在别的类别要逐个列出的位置下() {
        let home = Path::new("/Users/me");
        let rules = default_rules(home);
        let listed: Vec<&Rule> = rules.iter().filter(|r| r.only.is_empty() && r.root != home).collect();
        for r in rules.iter().filter(|r| !r.only.is_empty()) {
            for rel in r.only {
                let p = r.root.join(rel);
                let covered = listed.iter().any(|l| {
                    let first = p.strip_prefix(&l.root).ok().and_then(|rest| rest.components().next());
                    first.is_some_and(|c| !l.skip_prefixes.iter().any(|s| c.as_os_str().to_string_lossy().starts_with(s)))
                });
                assert!(!covered, "{} 会被算两遍", p.display());
            }
        }
    }

    #[test]
    fn 同一路径只算在前面那一类() {
        let t = TempTree::new();
        t.file("dl/old/big.bin", 2_000_000);
        let old = Rule { only: &["dl/old"], ..rule(CategoryId::DevCache, t.0.clone()) };
        let again = Rule { only: &["dl/old"], ..rule(CategoryId::AiCache, t.0.clone()) };
        let r = run(&[old, again]);
        assert_eq!((r.categories[0].items.len(), r.categories[1].items.len(), r.categories[1].total_bytes), (1, 0, 0));
    }

    #[test]
    fn 重复文件每组留一份_不碰小文件和硬链接() {
        let t = TempTree::new();
        let same = |rel: &str| {
            let p = t.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, vec![3u8; 1_500_000]).unwrap();
            p
        };
        let dup = same("Downloads/a copy.mov");
        let keep = same("Documents/a.mov");
        let long_ago = SystemTime::now() - Duration::from_secs(400 * 86400);
        fs::File::open(&keep).unwrap().set_times(fs::FileTimes::new().set_accessed(long_ago)).unwrap();
        // 同样大小、开头一样，只有结尾不同：不算重复
        let mut tail = vec![3u8; 1_500_000];
        tail[1_499_999] = 9;
        fs::write(t.file("Desktop/b.mov", 1), &tail).unwrap();
        t.file("Documents/s1", 10);
        t.file("Downloads/s2", 10);
        // 硬链接、隐藏目录里的、包里的都不算
        let linked = same("Desktop/h1.mov");
        fs::hard_link(&linked, t.0.join("Desktop/h2.mov")).unwrap();
        same("Documents/.hidden/c.mov");
        same("Documents/X.app/c.mov");
        let c = run(&[rule(CategoryId::Duplicates, t.0.clone())]).categories.remove(0);
        let names: Vec<_> = c.items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(names, [dup], "优先留不在下载里的那份");
        assert!(!c.partial && c.removal == crate::remove::Mode::Trash && c.only == ["Downloads/a copy.mov"]);
        let at = keep.symlink_metadata().unwrap().accessed().unwrap();
        assert!(at <= long_ago + Duration::from_secs(1), "读过内容后把访问时间改回去了");
    }

    #[test]
    fn 大文件和很久没打开的文件() {
        let t = TempTree::new();
        let big = t.file("Movies/big.mov", 51_000_000);
        // 稀疏文件不占空间，按实际占用算不够大
        fs::File::create(t.0.join("Movies/sparse.mov")).unwrap().set_len(60_000_000).unwrap();
        let old = t.file("Documents/old.zip", 12_000_000);
        let long_ago = SystemTime::now() - Duration::from_secs(400 * 86400);
        fs::File::options().write(true).open(&old).unwrap().set_times(fs::FileTimes::new().set_accessed(long_ago).set_modified(long_ago)).unwrap();
        t.file("Documents/new.zip", 12_000_000);
        fs::File::open(t.0.join("Documents")).unwrap().set_times(fs::FileTimes::new().set_accessed(long_ago)).unwrap();
        let c = run(&[rule(CategoryId::LargeFiles, t.0.clone())]).categories.remove(0);
        let names: Vec<_> = c.items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(names, [big, old]);
        assert!(c.removal == crate::remove::Mode::Trash && !c.partial);
        let at = t.0.join("Documents").symlink_metadata().unwrap().accessed().unwrap();
        assert!(at <= long_ago + Duration::from_secs(1), "读过目录后把访问时间改回去了");
    }

    #[test]
    fn 解析_brew_cleanup_的输出() {
        let cellar = Path::new("/opt/homebrew/Cellar");
        let out = "Would remove: /opt/homebrew/Cellar/node/20.1.0 (2,345 files, 60MB)\n\
                   Would remove: /Users/me/Library/Caches/Homebrew/downloads/x--1.0.tar.gz (1.2MB)\n\
                   Would remove: /opt/homebrew/Cellar/python@3.11 (3 files, 1KB)\n\
                   Would remove: /opt/homebrew/Cellar/a/../../etc (1 file)\n\
                   Warning: Skipping foo: most recent version 2 not installed\n\
                   Would remove: /opt/homebrew/Cellar/open ssl/3.0 (10 files, 5MB)";
        assert_eq!(parse_brew_cleanup(out, cellar), [cellar.join("node/20.1.0"), cellar.join("open ssl/3.0")]);
    }
}
