//! 已安装的应用和它们的残留文件。只读；真正的卸载在 `remove.rs`。

use crate::model::{AppInfo, Kept, Leftover};
use std::collections::{HashMap, HashSet};
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// 系统索引里没有大小的应用，才自己遍历去量，每个最多花这么久。
const APP_SIZE_BUDGET: Duration = Duration::from_secs(2);
/// 量一个应用的残留文件最多花这么久。
pub const LEFTOVER_BUDGET: Duration = Duration::from_secs(15);

/// 系统搜索索引里关于一个应用的记录。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Indexed {
    pub last_used_ms: Option<u64>,
    pub size_bytes: Option<u64>,
}

/// 应用可能留下东西的地方：资源库下的这些目录。
const LEFTOVER_DIRS: [&str; 10] = [
    "Application Support",
    "Caches",
    "Preferences",
    "Logs",
    "Saved Application State",
    "HTTPStorages",
    "WebKit",
    "Containers",
    "Application Scripts",
    "Cookies",
];

/// 系统保护的文件带这个标记，删不掉，也不该出现在卸载列表里。
const SF_RESTRICTED: u32 = 0x0008_0000;

pub struct Roots {
    /// 存放应用的目录
    pub apps: Vec<PathBuf>,
    /// 用户的资源库
    pub library: PathBuf,
    /// 整台电脑的资源库（`/Library`）。这里的东西要管理员权限才能删，卸载时只列出来，不动
    pub system_library: PathBuf,
}

pub fn default_roots(home: &Path) -> Roots {
    Roots { apps: vec![PathBuf::from("/Applications"), home.join("Applications")], library: home.join("Library"), system_library: PathBuf::from("/Library") }
}

/// 包标识、应用组这些名字之后要拿来拼路径，含斜杠、为空或以点开头的一律不认。
fn safe_name(s: &str) -> bool {
    !s.is_empty() && !s.contains('/') && !s.starts_with('.')
}

fn read_plist(app: &Path) -> Option<plist::Dictionary> {
    plist::Value::from_file(app.join("Contents/Info.plist")).ok()?.into_dictionary()
}

fn id_in(dict: &plist::Dictionary) -> Option<String> {
    dict.get("CFBundleIdentifier").and_then(|v| v.as_string()).filter(|s| safe_name(s)).map(str::to_owned)
}

/// 应用包里附带的应用扩展（`Contents/PlugIns/*.appex`）和登录项助手（`Contents/Library/LoginItems/*.app`）的包标识。
/// 苹果的不认：万一哪个应用里带了，不能顺手把系统的数据当成它的。
fn extra_ids(app: &Path) -> Vec<String> {
    [("Contents/PlugIns", "appex"), ("Contents/Library/LoginItems", "app")]
        .iter()
        .flat_map(|(dir, ext)| std::fs::read_dir(app.join(dir)).into_iter().flatten().flatten().map(|e| e.path()).filter(move |p| p.extension().is_some_and(|x| x == *ext)))
        .filter_map(|p| id_in(&read_plist(&p)?))
        .filter(|id| !id.starts_with("com.apple."))
        .collect()
}

/// 签名权限里声明的应用组（`Group Containers` 下的目录名）。没签名或读不到时为空。
fn app_groups(app: &Path) -> Vec<String> {
    // 测试里的假应用没有签名，从一个文本文件读
    #[cfg(test)]
    if let Ok(s) = std::fs::read_to_string(app.join("Contents/test-groups")) {
        return s.lines().map(str::to_owned).collect();
    }
    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::{CFString, CFStringRef};
    use core_foundation::url::{CFURLRef, CFURL};
    #[link(name = "Security", kind = "framework")]
    extern "C" {
        fn SecStaticCodeCreateWithPath(path: CFURLRef, flags: u32, code: *mut *const std::ffi::c_void) -> i32;
        fn SecCodeCopySigningInformation(code: *const std::ffi::c_void, flags: u32, info: *mut CFDictionaryRef) -> i32;
        static kSecCodeInfoEntitlementsDict: CFStringRef;
    }
    /// kSecCSSigningInformation
    const SIGNING_INFORMATION: u32 = 1 << 1;
    let Some(url) = CFURL::from_path(app, true) else { return vec![] };
    // SAFETY: 两个都是 Security 框架公开的函数，签名与这里一致；返回非 0 或空指针时直接放弃。
    // 拿到的对象按「Create/Copy 归调用方」的规则交给包装类型管理，字典里取出的值按「Get」规则只借用。
    unsafe {
        let mut code = std::ptr::null();
        if SecStaticCodeCreateWithPath(url.as_concrete_TypeRef(), 0, &mut code) != 0 || code.is_null() {
            return vec![];
        }
        let code = CFType::wrap_under_create_rule(code);
        let mut info: CFDictionaryRef = std::ptr::null();
        if SecCodeCopySigningInformation(code.as_CFTypeRef(), SIGNING_INFORMATION, &mut info) != 0 || info.is_null() {
            return vec![];
        }
        let info: CFDictionary<CFString, CFType> = CFDictionary::wrap_under_create_rule(info);
        let Some(ent) = info.find(CFString::wrap_under_get_rule(kSecCodeInfoEntitlementsDict)).and_then(|v| v.downcast::<CFDictionary>()) else { return vec![] };
        let ent: CFDictionary<CFString, CFType> = CFDictionary::wrap_under_get_rule(ent.as_concrete_TypeRef());
        let Some(groups) = ent.find(CFString::new("com.apple.security.application-groups")).and_then(|v| v.downcast::<CFArray>()) else { return vec![] };
        let groups: CFArray<CFType> = CFArray::wrap_under_get_rule(groups.as_concrete_TypeRef());
        groups.iter().filter_map(|v| v.downcast::<CFString>().map(|s| s.to_string())).collect()
    }
}

/// 一个已安装的应用包里、认残留要用的信息。必须在应用被移走之前读。
pub struct Bundle {
    pub path: PathBuf,
    pub bundle_id: String,
    pub version: String,
    pub name: String,
    /// 包里应用扩展和登录项助手的包标识
    extra_ids: Vec<String>,
    /// 签名权限里声明的应用组
    groups: Vec<String>,
    /// 用户资源库里、启动的程序在这个应用包里的启动代理配置文件
    agents: Vec<PathBuf>,
    /// `/Library` 下属于它的启动代理、守护进程和特权助手：要管理员权限才能删，只列出来
    admin: Vec<PathBuf>,
}

/// 读一个应用包。读不出包标识的不认。
fn read_bundle(path: &Path, system_library: &Path) -> Option<Bundle> {
    let dict = read_plist(path)?;
    let bundle_id = id_in(&dict)?;
    let version = dict.get("CFBundleShortVersionString").and_then(|v| v.as_string()).unwrap_or_default().to_owned();
    // 老式的特权助手在说明文件里登记（键是助手的标签），装在固定的两处
    let mut admin = Vec::new();
    for helper in dict.get("SMPrivilegedExecutables").and_then(|v| v.as_dictionary()).into_iter().flat_map(|d| d.keys()).filter(|k| safe_name(k)) {
        admin.push(system_library.join("PrivilegedHelperTools").join(helper));
        admin.push(system_library.join("LaunchDaemons").join(format!("{helper}.plist")));
    }
    Some(Bundle {
        name: path.file_stem().unwrap_or_default().to_string_lossy().into_owned(),
        extra_ids: extra_ids(path),
        groups: app_groups(path),
        path: path.to_path_buf(),
        bundle_id,
        version,
        agents: vec![],
        admin,
    })
}

/// 一个目录里的启动项配置文件 → 它启动的程序（`Program`，没有时取 `ProgramArguments` 的第一项）。
fn launchd_jobs(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "plist") && p.symlink_metadata().is_ok_and(|m| m.is_file()))
        .filter_map(|p| {
            let dict = plist::Value::from_file(&p).ok()?.into_dictionary()?;
            let program = dict.get("Program").and_then(|v| v.as_string()).or_else(|| dict.get("ProgramArguments")?.as_array()?.first()?.as_string())?;
            Some((p, PathBuf::from(program)))
        })
        .collect()
}

/// 分几个线程对每一项做 `f`，结果保持原来的顺序。
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let chunk = items.len().div_ceil(8).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = items.chunks(chunk).map(|part| s.spawn(|| part.iter().map(&f).collect::<Vec<_>>())).collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

/// 应用目录里装着的全部应用包，包括列表里不显示的（系统保护的、本应用自己、符号链接）：
/// 判断残留是不是和别的应用共用时，它们都要算上。读签名每个要几十毫秒，所以分几个线程读。
pub fn installed(roots: &Roots) -> Vec<Bundle> {
    let paths: Vec<PathBuf> = roots
        .apps
        .iter()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "app"))
        .collect();
    let mut all: Vec<Bundle> = par_map(&paths, |p| read_bundle(p, &roots.system_library)).into_iter().flatten().collect();
    // 启动项只认「启动的程序就在这个应用包里」的，不按标签的前缀猜（com.google.Chrome.canary 是另一个应用）
    let user_jobs = launchd_jobs(&roots.library.join("LaunchAgents"));
    let system_jobs: Vec<_> = ["LaunchAgents", "LaunchDaemons"].iter().flat_map(|d| launchd_jobs(&roots.system_library.join(d))).collect();
    for b in &mut all {
        b.agents = user_jobs.iter().filter(|(_, program)| program.starts_with(&b.path)).map(|(plist, _)| plist.clone()).collect();
        b.admin.extend(system_jobs.iter().filter(|(_, program)| program.starts_with(&b.path)).map(|(plist, _)| plist.clone()));
    }
    all
}

/// 一个应用**可能**留下的、卸载时可以移走的全部路径。列出残留和卸载前的守卫用的是同一份，
/// 所以卸载时只可能碰到这里算出来的路径。
///
/// 只按包标识精确匹配（主程序的，以及包里扩展和登录项助手的）；按应用名匹配只用在两个目录里，而且必须完全同名。
/// 应用组按签名里声明的完整组名匹配；启动代理只认启动的程序在这个应用包里的。
/// 宁可漏掉一些残留，也不误删名字相近的别家数据。
pub fn leftover_candidates(library: &Path, b: &Bundle) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for (i, bundle_id) in std::iter::once(&b.bundle_id).chain(&b.extra_ids).enumerate() {
        for dir in LEFTOVER_DIRS {
            let base = library.join(dir);
            match dir {
                "Preferences" => out.push(base.join(format!("{bundle_id}.plist"))),
                "Saved Application State" => out.push(base.join(format!("{bundle_id}.savedState"))),
                "Cookies" => out.push(base.join(format!("{bundle_id}.binarycookies"))),
                _ => out.push(base.join(bundle_id)),
            }
            if i == 0 && matches!(dir, "Application Support" | "Logs") && safe_name(&b.name) && &b.name != bundle_id {
                out.push(base.join(&b.name));
            }
        }
    }
    // 苹果的应用组可能和系统应用（不在应用目录里，查不到）共用，不碰
    out.extend(b.groups.iter().filter(|g| safe_name(g) && !g.contains("com.apple.")).map(|g| library.join("Group Containers").join(g)));
    out.extend(b.agents.iter().cloned());
    out
}

/// `all[i]` 实际存在的残留：（卸载时移走的，保留的）。`all` 是 [`installed`] 的结果。
/// 别的已装应用也会认的（同一包标识、同名、同一应用组），都保留并写明和谁共用：
/// 同一个应用装了两份（比如新旧两版）时，卸载旧的那份不能把新版正在用的数据一起带走。
pub fn leftovers_of(roots: &Roots, all: &[Bundle], i: usize) -> (Vec<PathBuf>, Vec<Kept>) {
    let others: HashMap<PathBuf, &str> =
        all.iter().enumerate().filter(|(j, _)| *j != i).flat_map(|(_, o)| leftover_candidates(&roots.library, o).into_iter().map(move |p| (p, o.name.as_str()))).collect();
    let (mut mine, mut kept): (Vec<PathBuf>, Vec<Kept>) = (Vec::new(), Vec::new());
    let skip = |mine: &[PathBuf], kept: &[Kept], p: &PathBuf| mine.contains(p) || kept.iter().any(|k| &k.path == p) || p.symlink_metadata().is_err();
    for path in leftover_candidates(&roots.library, &all[i]) {
        if skip(&mine, &kept, &path) {
            continue;
        }
        match others.get(&path) {
            Some(other) => kept.push(Kept { path, shared_with: Some((*other).to_owned()) }),
            None => mine.push(path),
        }
    }
    for path in &all[i].admin {
        if !skip(&mine, &kept, path) {
            kept.push(Kept { path: path.clone(), shared_with: None });
        }
    }
    (mine, kept)
}

/// 刚被拖进废纸篓的应用 `app` 留下的数据：（应用名，可以移走的路径）。读不出包标识时返回 None。
/// 和卸载用同一套规则（[`leftovers_of`]）：别的已装应用也认的一律不算，所以自动更新把旧版移进废纸篓、
/// 新版还装着时，什么都不会列出来。
pub fn trashed_leftovers(roots: &Roots, app: &Path) -> Option<(String, Vec<PathBuf>)> {
    let mut all = installed(roots);
    all.push(read_bundle(app, &roots.system_library)?);
    let i = all.len() - 1;
    let (mine, _) = leftovers_of(roots, &all, i);
    Some((all[i].name.clone(), mine))
}

/// 量一个应用的残留文件有多大，整体不超过 `budget`。
pub fn measure(app: &mut AppInfo, budget: Duration) {
    let deadline = Instant::now() + budget;
    let mut complete = true;
    for l in &mut app.leftovers {
        let dev = l.path.symlink_metadata().map(|m| m.dev()).unwrap_or(0);
        let (size, done) = crate::scan::disk_usage_until(&l.path, dev, Some(deadline));
        l.size_bytes = Some(size);
        complete &= done;
    }
    app.leftover_bytes = Some(app.leftovers.iter().filter_map(|l| l.size_bytes).sum());
    app.leftover_complete = complete;
}

/// 已删除应用留下的数据只在这两处找：都是一个应用一份、以包标识命名，和应用包一一对应。
/// Application Support、Preferences 这些地方命令行工具和别的程序也会写，Group Containers 是几个应用共用的，
/// 认错的风险太大，都不找（宁可漏掉）。
const ORPHAN_DIRS: [(&str, &str); 2] = [("Containers", ""), ("Saved Application State", ".savedState")];
/// 最近这么久内改动过的不算：应用可能装在没插上的外接盘里，最近还在用。
const ORPHAN_MIN_AGE: Duration = Duration::from_secs(30 * 86_400);

/// 系统搜索索引里的全部应用和应用扩展。查不了时返回 None。
pub fn spotlight_apps() -> Option<Vec<PathBuf>> {
    let out = std::process::Command::new("/usr/bin/mdfind")
        .arg("kMDItemContentType == \"com.apple.application-bundle\" || kMDItemContentType == \"com.apple.application-and-system-extension\"")
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).lines().map(PathBuf::from).collect())
}

/// 本机认得的全部包标识：应用目录里的应用（`all`）和搜索索引里找得到的所有应用，连同它们的扩展和登录项助手。
/// 索引关了或者明显没建好（找到的还不到应用目录里的一半）时返回 None：这时什么都不能算作已删除应用的。
fn known_ids(all: &[Bundle], spotlight: &dyn Fn() -> Option<Vec<PathBuf>>) -> Option<HashSet<String>> {
    let found = spotlight()?;
    if found.is_empty() || found.len() * 2 < all.len() {
        return None;
    }
    let mut ids: HashSet<String> = all.iter().flat_map(|b| std::iter::once(b.bundle_id.clone()).chain(b.extra_ids.iter().cloned())).collect();
    for p in &found {
        ids.extend(read_plist(p).as_ref().and_then(id_in));
        ids.extend(extra_ids(p));
    }
    // 包标识不分大小写
    Some(ids.into_iter().map(|id| id.to_ascii_lowercase()).collect())
}

/// 像包标识的名字：至少三段、只有字母数字和 `.-_`。
fn looks_like_id(s: &str) -> bool {
    s.split('.').count() >= 3 && s.split('.').all(|part| !part.is_empty()) && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// 包标识的前两段，大致是开发者（`com.microsoft.Word` → `com.microsoft`）。
fn vendor(id: &str) -> &str {
    id.match_indices('.').nth(1).map_or(id, |(i, _)| &id[..i])
}

/// 以开发团队编号开头的名字（`FN2V63AD2J.com.tencent.ScreenCapture3`）：多半是还装着的应用深处带的助手程序。
fn team_prefixed(s: &str) -> bool {
    s.split('.').next().is_some_and(|first| first.len() == 10 && first.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()))
}

/// 已删除应用留下的数据（规格：宁可漏掉）。只读，不量大小。
/// 只认：像包标识、不是苹果的、不以团队编号开头、30 天内没改动过，而且同一开发者（包标识前两段相同）
/// 的应用一个都没装着的。应用包深处带的助手程序（扩展、截图插件等）也有自己的容器，
/// 系统索引不一定找得到它们，所以只要同一开发者还有应用装着，就不认。
fn orphans(library: &Path, known: &HashSet<String>, now: SystemTime) -> Vec<PathBuf> {
    let vendors: HashSet<&str> = known.iter().map(|k| vendor(k)).collect();
    let modified = |p: &Path| p.symlink_metadata().and_then(|m| m.modified()).ok();
    let mut out = Vec::new();
    for (dir, suffix) in ORPHAN_DIRS {
        let Ok(entries) = std::fs::read_dir(library.join(dir)) else { continue };
        for path in entries.flatten().map(|e| e.path()) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let Some(id) = name.strip_suffix(suffix) else { continue };
            if team_prefixed(id) {
                continue;
            }
            let id = id.to_ascii_lowercase();
            if !looks_like_id(&id) || id.starts_with("com.apple.") || vendors.contains(vendor(&id)) || !path.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                continue;
            }
            // 容器自己的目录很少变，里面的 Data 才是应用一直在写的
            let newest = [modified(&path), modified(&path.join("Data"))].into_iter().flatten().max();
            if newest.is_some_and(|t| now.duration_since(t).unwrap_or_default() >= ORPHAN_MIN_AGE) {
                out.push(path);
            }
        }
    }
    out
}

/// 重新找一遍已删除应用留下的数据，只给路径。卸载前的守卫用它。
pub fn orphan_paths(roots: &Roots, spotlight: &dyn Fn() -> Option<Vec<PathBuf>>, now: SystemTime) -> Vec<PathBuf> {
    known_ids(&installed(roots), spotlight).map(|known| orphans(&roots.library, &known, now)).unwrap_or_default()
}

/// 找已删除应用留下的数据并量大小（总共最多 [`LEFTOVER_BUDGET`]），按大小降序。
pub fn orphan_leftovers(roots: &Roots, spotlight: &dyn Fn() -> Option<Vec<PathBuf>>, now: SystemTime) -> Vec<Leftover> {
    let deadline = Instant::now() + LEFTOVER_BUDGET;
    let mut out: Vec<Leftover> = orphan_paths(roots, spotlight, now)
        .into_iter()
        .map(|path| {
            let dev = path.symlink_metadata().map(|m| m.dev()).unwrap_or(0);
            let size = crate::scan::disk_usage_until(&path, dev, Some(deadline)).0;
            Leftover { path, size_bytes: Some(size) }
        })
        .collect();
    out.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes));
    out
}

/// 向系统的搜索索引一次问完所有应用的大小和上次打开时间。
/// 实测 138 个应用 0.17 秒；自己逐个遍历应用包去量要 7 分多钟。
pub fn spotlight_index(apps: &[PathBuf]) -> HashMap<PathBuf, Indexed> {
    let mut out = HashMap::new();
    if apps.is_empty() {
        return out;
    }
    let paths: Vec<String> = apps.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let mut args = vec!["-name", "kMDItemLastUsedDate", "-name", "kMDItemPhysicalSize", "-raw", "-nullMarker", "-"];
    args.extend(paths.iter().map(String::as_str));
    // 索引正在重建时可能很久不回，最多等一会儿，等不到就当作索引里没有
    let Ok(text) = crate::devenv::run(Path::new("/usr/bin/mdls"), &args, Duration::from_secs(10)) else {
        return out;
    };
    // -raw 输出的各项之间用 NUL 分隔：每个应用两项，按属性名的字母顺序排（上次使用时间在前，大小在后）
    let fields: Vec<&str> = text.split('\0').collect();
    for (app, pair) in apps.iter().zip(fields.chunks(2)) {
        if let [used, size] = pair {
            out.insert(app.clone(), Indexed { last_used_ms: parse_date(used.trim()), size_bytes: size.trim().parse().ok() });
        }
    }
    out
}

/// 解析 `2026-10-12 08:11:22 +0000` 这样的时间，得到毫秒时间戳。
fn parse_date(s: &str) -> Option<u64> {
    let (date, rest) = s.split_once(' ')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = rest.split(' ').next()?.split(':').map(|x| x.parse::<i64>());
    let (h, min, sec) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    // 公历日期换算成自 1970-01-01 起的天数
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2.rem_euclid(400);
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    u64::try_from((days * 86_400 + h * 3600 + min * 60 + sec) * 1000).ok()
}

/// 列出可以卸载的应用，按大小降序。残留文件只找出来，不量大小（见 [`measure`]）。
/// `index` 和 `running` 由调用方提供，正式运行时查系统，测试时传假的。
pub fn list(roots: &Roots, index: &dyn Fn(&[PathBuf]) -> HashMap<PathBuf, Indexed>, running: &(dyn Fn(&Path) -> (bool, Vec<String>) + Sync)) -> Vec<AppInfo> {
    let own = std::env::current_exe().ok();
    let all = installed(roots);
    // 符号链接、系统保护的应用、本应用自己，都不列
    let found: Vec<usize> = (0..all.len())
        .filter(|&i| {
            let path = &all[i].path;
            path.symlink_metadata().is_ok_and(|meta| meta.is_dir() && meta.st_flags() & SF_RESTRICTED == 0) && !own.as_ref().is_some_and(|exe| exe.starts_with(path))
        })
        .collect();
    let paths: Vec<PathBuf> = found.iter().map(|&i| all[i].path.clone()).collect();
    let indexed = index(&paths);

    // 索引里没有大小的应用要自己去量，分几个线程做
    let mut apps: Vec<AppInfo> = par_map(&found, |&i| {
        let b = &all[i];
        let path = &b.path;
        let known = indexed.get(path).copied().unwrap_or_default();
        let size_bytes = known.size_bytes.unwrap_or_else(|| {
            let dev = path.symlink_metadata().map(|m| m.dev()).unwrap_or(0);
            crate::scan::disk_usage_until(path, dev, Some(Instant::now() + APP_SIZE_BUDGET)).0
        });
        let (leftovers, kept) = leftovers_of(roots, &all, i);
        let (running, tools_running) = running(path);
        AppInfo {
            id: path.display().to_string(),
            size_bytes,
            last_used_ms: known.last_used_ms,
            created_ms: path.metadata().and_then(|m| m.created()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64),
            leftover_bytes: None,
            leftover_complete: false,
            leftovers: leftovers.into_iter().map(|path| Leftover { path, size_bytes: None }).collect(),
            kept,
            running,
            tools_running,
            name: b.name.clone(),
            bundle_id: b.bundle_id.clone(),
            version: b.version.clone(),
            path: path.clone(),
        }
    });
    apps.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.name.cmp(&b.name)));
    apps
}

/// 是不是这个应用的主程序：直接放在 `Contents/MacOS` 下的那个。
fn is_main(exe: &Path, app: &Path) -> bool {
    exe.parent() == Some(app.join("Contents/MacOS").as_path())
}

/// 查一个应用包里有什么在运行：（主程序在不在运行，包里其他在运行的进程名）。
/// 只有附带的命令行工具在跑（比如 Xcode 里的 git、clang）不算应用开着，但卸载前也会请它们退出，所以要告诉用户。
pub fn running_checker() -> impl Fn(&Path) -> (bool, Vec<String>) + Sync {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
    );
    let procs: Vec<(PathBuf, String)> = sys.processes().values().filter_map(|p| Some((p.exe()?.to_path_buf(), p.name().to_string_lossy().into_owned()))).collect();
    move |app: &Path| {
        let main = procs.iter().any(|(exe, _)| is_main(exe, app));
        let mut tools: Vec<String> = procs.iter().filter(|(exe, _)| exe.starts_with(app) && !is_main(exe, app)).map(|(_, name)| name.clone()).collect();
        tools.sort();
        tools.dedup();
        (main, tools)
    }
}

/// 应用包里正在运行的进程：(进程号, 是不是应用的主程序)。主程序是直接放在 `Contents/MacOS` 下的那个。
fn processes_in(app: &Path) -> Vec<(u32, bool)> {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
    );
    let main_dir = app.join("Contents/MacOS");
    sys.processes()
        .values()
        .filter_map(|p| p.exe().filter(|exe| exe.starts_with(app)).map(|exe| (p.pid().as_u32(), exe.parent() == Some(main_dir.as_path()))))
        .collect()
}

/// 请一个应用正常退出，效果等同于用户选了它的「退出」菜单：有没保存的内容时它会先问用户，所以可能不退出。
/// 进程不是一个应用（比如命令行辅助程序）时返回 false。
pub fn request_quit(pid: u32) -> bool {
    use objc2::runtime::{AnyClass, AnyObject};
    objc2::rc::autoreleasepool(|_| {
        let Some(class) = AnyClass::get(c"NSRunningApplication") else { return false };
        // SAFETY: 两个都是 AppKit 公开的方法，签名与这里一致；查不到进程时返回空指针，下面判了空。
        unsafe {
            let running: *mut AnyObject = objc2::msg_send![class, runningApplicationWithProcessIdentifier: pid as libc::pid_t];
            !running.is_null() && objc2::msg_send![running, terminate]
        }
    })
}

/// 卸载前请正在运行的应用退出，并等它真的退出；返回是否已经退干净。
/// 只请主程序正常退出，不强杀：它因为有没保存的内容而不肯退时，这里等到超时返回 false，由调用方跳过它。
/// 主程序退了之后还赖着的辅助进程，再单独请它们退出一次。
pub fn quit_and_wait(app: &Path, timeout: Duration) -> bool {
    let ask = |pids: &[u32]| {
        for &pid in pids {
            if !request_quit(pid) {
                let _ = crate::system::quit(pid);
            }
        }
    };
    let pids = |main: bool| processes_in(app).into_iter().filter(|p| p.1 == main).map(|p| p.0).collect::<Vec<_>>();
    let mains = pids(true);
    ask(&mains);
    let mut asked_helpers = false;
    let start = Instant::now();
    loop {
        let (mains, helpers) = (pids(true), pids(false));
        if mains.is_empty() && helpers.is_empty() {
            return true;
        }
        if mains.is_empty() && !asked_helpers {
            ask(&helpers);
            asked_helpers = true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
pub(crate) mod testutil {
    use super::*;
    use crate::scan::testutil::TempTree;

    /// 在临时目录里搭一个假应用和它的残留，返回（应用路径，根目录集合）。
    pub fn fake_app(t: &TempTree, name: &str, bundle_id: &str) -> (PathBuf, Roots) {
        let app = t.0.join(format!("Applications/{name}.app"));
        t.file(&format!("Applications/{name}.app/Contents/MacOS/{name}"), 300_000);
        let info = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{bundle_id}</string><key>CFBundleShortVersionString</key><string>1.2.3</string></dict></plist>"#
        );
        std::fs::write(app.join("Contents/Info.plist"), info).unwrap();
        (app, Roots { apps: vec![t.0.join("Applications")], library: t.0.join("Library"), system_library: t.0.join("SystemLibrary") })
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::fake_app;
    use super::*;
    use crate::scan::testutil::TempTree;

    #[test]
    fn 拖进废纸篓的应用_只列没有别人认的残留() {
        let t = TempTree::new();
        let (app, roots) = fake_app(&t, "Demo", "com.example.demo");
        t.file("Library/Caches/com.example.demo/c.bin", 50_000);
        t.file("Library/Preferences/com.example.demo.plist", 100);
        // 用户把它拖进了废纸篓
        let trashed = t.0.join(".Trash/Demo.app");
        std::fs::create_dir_all(trashed.parent().unwrap()).unwrap();
        std::fs::rename(&app, &trashed).unwrap();
        let (name, mut paths) = trashed_leftovers(&roots, &trashed).unwrap();
        paths.sort();
        assert_eq!(name, "Demo");
        assert_eq!(paths, vec![t.0.join("Library/Caches/com.example.demo"), t.0.join("Library/Preferences/com.example.demo.plist")]);
        // 自动更新：旧版进了废纸篓，新版还装着，数据是新版在用的
        fake_app(&t, "Demo", "com.example.demo");
        assert_eq!(trashed_leftovers(&roots, &trashed).unwrap().1, Vec::<PathBuf>::new());
        assert!(trashed_leftovers(&roots, &t.0.join(".Trash/nothing.app")).is_none());
    }

    #[test]
    fn 列出应用及其残留文件() {
        let t = TempTree::new();
        let (app, roots) = fake_app(&t, "Demo", "com.example.demo");
        t.file("Library/Caches/com.example.demo/c.bin", 50_000);
        t.file("Library/Preferences/com.example.demo.plist", 100);
        t.file("Library/Application Support/Demo/data.db", 20_000);
        // 名字相近但不是它的
        t.file("Library/Caches/com.example.demo2/c.bin", 10);
        t.file("Library/Application Support/Demo Helper/x", 10);
        // 不是应用、没有说明文件的，都不列
        t.file("Applications/readme.txt", 10);
        t.file("Applications/Broken.app/Contents/MacOS/x", 10);

        let when = 1_700_000_000_000;
        let index = |paths: &[PathBuf]| paths.iter().map(|p| (p.clone(), Indexed { last_used_ms: Some(when), size_bytes: None })).collect();
        let mut apps = list(&roots, &index, &|_| (false, vec![]));
        assert_eq!(apps.len(), 1);
        assert_eq!((apps[0].leftover_bytes, apps[0].leftovers[0].size_bytes), (None, None), "列出时不量残留");
        measure(&mut apps[0], Duration::from_secs(5));
        let a = &apps[0];
        assert_eq!((a.name.as_str(), a.bundle_id.as_str(), a.version.as_str()), ("Demo", "com.example.demo", "1.2.3"));
        assert_eq!((a.id.clone(), a.last_used_ms, a.running), (app.display().to_string(), Some(when), false));
        assert!(a.size_bytes >= 300_000, "索引里没有大小时自己量");
        let mut names: Vec<String> = a.leftovers.iter().map(|l| l.path.strip_prefix(&roots.library).unwrap().display().to_string()).collect();
        names.sort();
        assert_eq!(names, ["Application Support/Demo", "Caches/com.example.demo", "Preferences/com.example.demo.plist"]);
        assert!(a.leftover_complete && a.leftover_bytes.unwrap() >= 70_000);
        assert_eq!(a.leftover_bytes, Some(a.leftovers.iter().filter_map(|l| l.size_bytes).sum::<u64>()));

        // 索引里有大小时直接用，不自己去量
        let sized = |paths: &[PathBuf]| paths.iter().map(|p| (p.clone(), Indexed { last_used_ms: None, size_bytes: Some(42) })).collect();
        assert_eq!(list(&roots, &sized, &|_| (false, vec![]))[0].size_bytes, 42);
    }

    #[test]
    fn 包标识含斜杠的应用不认() {
        let t = TempTree::new();
        let (_, roots) = fake_app(&t, "Evil", "../../etc");
        assert!(list(&roots, &|_| HashMap::new(), &|_| (false, vec![])).is_empty());
    }

    #[test]
    fn 量大小到了时限就停() {
        let t = TempTree::new();
        for i in 0..2000 {
            t.file(&format!("big/{i}"), 1);
        }
        let dev = t.0.symlink_metadata().unwrap().dev();
        let (_, done) = crate::scan::disk_usage_until(&t.0.join("big"), dev, Some(Instant::now()));
        assert!(!done);
        assert!(crate::scan::disk_usage_until(&t.0.join("big"), dev, None).1);
    }

    #[test]
    fn 包里附带的工具在跑不算应用开着() {
        let app = Path::new("/Applications/Xcode.app");
        assert!(is_main(Path::new("/Applications/Xcode.app/Contents/MacOS/Xcode"), app));
        assert!(!is_main(Path::new("/Applications/Xcode.app/Contents/Developer/usr/bin/make"), app));
        // 测试程序自己就是「包」里（它所在的目录下）一个不是主程序的进程
        let dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let (main, tools) = running_checker()(&dir);
        assert!(!main && !tools.is_empty());
    }

    fn rel(roots: &Roots, p: &Path) -> String {
        p.strip_prefix(&roots.library).or_else(|_| p.strip_prefix(&roots.system_library)).unwrap().display().to_string()
    }

    fn write_plist(path: &Path, pairs: Vec<(&str, plist::Value)>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        plist::Value::Dictionary(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()).to_file_xml(path).unwrap();
    }

    #[test]
    fn 和别的已装应用共用的残留只列出不算它的() {
        let t = TempTree::new();
        // 同一个应用的两份（改了名的新旧版本），包标识相同
        let (_, roots) = fake_app(&t, "starKnowledge", "com.example.km");
        fake_app(&t, "星识", "com.example.km");
        t.file("Library/Application Support/com.example.km/db", 10);
        t.file("Library/Preferences/com.example.km.plist", 10);
        t.file("Library/Application Support/starKnowledge/x", 10);
        let apps = list(&roots, &|_| HashMap::new(), &|_| (false, vec![]));
        let a = apps.iter().find(|a| a.name == "starKnowledge").unwrap();
        assert_eq!(a.leftovers.iter().map(|l| rel(&roots, &l.path)).collect::<Vec<_>>(), ["Application Support/starKnowledge"]);
        let mut kept: Vec<_> = a.kept.iter().map(|k| (rel(&roots, &k.path), k.shared_with.clone())).collect();
        kept.sort();
        assert_eq!(kept, [("Application Support/com.example.km".into(), Some("星识".into())), ("Preferences/com.example.km.plist".into(), Some("星识".into()))]);
    }

    #[test]
    fn 扩展容器_应用组_启动项也算残留_系统级的只列出() {
        let t = TempTree::new();
        let (app, roots) = fake_app(&t, "Office", "com.example.office");
        let s = |x: &str| plist::Value::String(x.into());
        write_plist(
            &app.join("Contents/Info.plist"),
            vec![("CFBundleIdentifier", s("com.example.office")), ("SMPrivilegedExecutables", plist::Value::Dictionary([("com.example.office.helper".to_owned(), s("anchor apple"))].into_iter().collect()))],
        );
        write_plist(&app.join("Contents/PlugIns/Finder.appex/Contents/Info.plist"), vec![("CFBundleIdentifier", s("com.example.office.FinderSync"))]);
        std::fs::write(app.join("Contents/test-groups"), "TEAM.office\nTEAM.suite\ngroup.com.apple.notes\n").unwrap();
        let (word, _) = fake_app(&t, "Word", "com.example.word");
        std::fs::write(word.join("Contents/test-groups"), "TEAM.suite\n").unwrap();
        for p in ["Containers/com.example.office.FinderSync/x", "Group Containers/TEAM.office/x", "Group Containers/TEAM.suite/x", "Group Containers/group.com.apple.notes/x"] {
            t.file(&format!("Library/{p}"), 10);
        }
        let inside = |p: &str| plist::Value::String(app.join(p).display().to_string());
        write_plist(&t.0.join("Library/LaunchAgents/com.example.office.updater.plist"), vec![("Program", inside("Contents/MacOS/updater"))]);
        write_plist(&t.0.join("Library/LaunchAgents/com.example.office.other.plist"), vec![("ProgramArguments", plist::Value::Array(vec![s("/usr/local/bin/other")]))]);
        t.file("SystemLibrary/PrivilegedHelperTools/com.example.office.helper", 10);
        write_plist(&t.0.join("SystemLibrary/LaunchDaemons/com.example.office.helper.plist"), vec![("Program", s("/Library/PrivilegedHelperTools/com.example.office.helper"))]);
        write_plist(&t.0.join("SystemLibrary/LaunchDaemons/com.example.office.daemon.plist"), vec![("ProgramArguments", plist::Value::Array(vec![inside("Contents/Library/daemon")]))]);

        let apps = list(&roots, &|_| HashMap::new(), &|_| (false, vec![]));
        let a = apps.iter().find(|a| a.name == "Office").unwrap();
        let mut mine: Vec<_> = a.leftovers.iter().map(|l| rel(&roots, &l.path)).collect();
        mine.sort();
        assert_eq!(mine, ["Containers/com.example.office.FinderSync", "Group Containers/TEAM.office", "LaunchAgents/com.example.office.updater.plist"]);
        let mut kept: Vec<_> = a.kept.iter().map(|k| (rel(&roots, &k.path), k.shared_with.clone())).collect();
        kept.sort();
        assert_eq!(
            kept,
            [
                ("Group Containers/TEAM.suite".into(), Some("Word".into())),
                ("LaunchDaemons/com.example.office.daemon.plist".into(), None),
                ("LaunchDaemons/com.example.office.helper.plist".into(), None),
                ("PrivilegedHelperTools/com.example.office.helper".into(), None),
            ]
        );
    }

    #[test]
    fn 已删除应用的残留_宁可漏掉() {
        let t = TempTree::new();
        let (app, roots) = fake_app(&t, "Demo", "com.example.demo");
        let s = |x: &str| plist::Value::String(x.into());
        write_plist(&app.join("Contents/PlugIns/Share.appex/Contents/Info.plist"), vec![("CFBundleIdentifier", s("io.demo.share"))]);
        write_plist(&t.0.join("Elsewhere/Spot.app/Contents/Info.plist"), vec![("CFBundleIdentifier", s("org.Spot.app"))]);
        for d in [
            "Containers/com.gone.app", // 应用已经不在了
            "Saved Application State/com.gone.app.savedState",
            "Containers/io.demo.share",         // 还装着的应用的扩展
            "Containers/com.example.helper",    // 同一开发者还有应用装着
            "Containers/org.spot.App.widget",   // 索引里找得到的应用的（大小写不同）
            "Containers/ABCDE12345.com.gone.x", // 以团队编号开头
            "Containers/com.apple.Notes",       // 苹果的
            "Containers/5C0983CC-6196-45A4-ABD3-5DCE776893EC",
            "Containers/gone.app",        // 不像包标识
            "Containers/net.gone.recent", // 最近还在写
        ] {
            std::fs::create_dir_all(t.0.join("Library").join(d).join("Data")).unwrap();
        }
        t.file("Library/Containers/net.gone.file", 10);
        let now = SystemTime::now() + Duration::from_secs(31 * 86_400);
        std::fs::File::open(t.0.join("Library/Containers/net.gone.recent/Data")).unwrap().set_modified(now - Duration::from_secs(86_400)).unwrap();
        let spot = t.0.join("Elsewhere/Spot.app");
        let spotlight = || Some(vec![spot.clone()]);

        let mut found: Vec<_> = orphan_paths(&roots, &spotlight, now).iter().map(|p| rel(&roots, p)).collect();
        found.sort();
        assert_eq!(found, ["Containers/com.gone.app", "Saved Application State/com.gone.app.savedState"]);
        // 搜索索引查不了或什么都没找到：一个都不算
        assert!(orphan_paths(&roots, &|| None, now).is_empty());
        assert!(orphan_paths(&roots, &|| Some(vec![]), now).is_empty());
        let measured = orphan_leftovers(&roots, &spotlight, now);
        assert!(measured.len() == 2 && measured.iter().all(|l| l.size_bytes.is_some()));
    }

    #[test]
    fn 解析系统给出的日期() {
        assert_eq!(parse_date("1970-01-01 00:00:00 +0000"), Some(0));
        assert_eq!(parse_date("2026-10-13 09:30:15 +0000"), Some(1_791_883_815_000));
        assert_eq!(parse_date("2024-02-29 12:00:00 +0000"), Some(1_709_208_000_000));
        assert_eq!(parse_date("-"), None);
    }
}

#[cfg(test)]
mod quit_tests {
    use super::*;

    /// 真的打开一个系统自带的应用再请它退出，所以默认不跑：
    /// `cargo test quits_a_running_app -- --ignored`
    #[test]
    #[ignore]
    fn quits_a_running_app() {
        let app = Path::new("/System/Applications/Chess.app");
        assert!(processes_in(app).is_empty(), "国际象棋本来就开着，不拿它做试验");
        assert!(std::process::Command::new("/usr/bin/open").args(["-g", "-a"]).arg(app).status().unwrap().success());
        let opened = Instant::now();
        while processes_in(app).is_empty() && opened.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(200));
        }
        assert_eq!(processes_in(app).iter().filter(|p| p.1).count(), 1, "应该正好有一个主程序在运行");
        // 刚启动的应用要过一会儿才开始处理「退出」请求
        std::thread::sleep(Duration::from_secs(2));
        assert!(quit_and_wait(app, Duration::from_secs(10)));
        assert!(processes_in(app).is_empty());
    }
}
