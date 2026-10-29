//! 删除：整个应用里唯一能删文件的地方（规格不变量 2、3）。
//! 只接受扫描报告里的条目编号，拒绝白名单以外的一切路径。

use crate::i18n::t;
use crate::admin::AdminError;
use crate::apps::{installed, leftovers_of, Roots};
use crate::model::{AppInfo, Category, CategoryId, CleanProgress, Failure, Item, RemoveResult, ScanReport, Trashed};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(serde::Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// 直接删除，立即释放空间
    Permanent,
    /// 移到废纸篓，清空后才释放
    Trash,
}

/// 删除方式由类别决定，调用方不能指定（规格 D-12）：
/// 能重新生成的直接删，否则不释放空间；个人文件进废纸篓，留一次反悔的机会。
pub fn mode_of(id: CategoryId) -> Mode {
    match id {
        CategoryId::AppCache | CategoryId::SystemCache | CategoryId::Logs | CategoryId::DevCache | CategoryId::AiCache | CategoryId::Trash | CategoryId::CrashDumps => Mode::Permanent,
        // 固件可以重新下载；Homebrew 旧版本是 brew 自己认定可以删的
        CategoryId::IosFirmware | CategoryId::HomebrewOld => Mode::Permanent,
        // 个人文件和项目目录只进废纸篓（规格 D-12）
        CategoryId::OldDownloads | CategoryId::StaleProjects | CategoryId::AiHistory => Mode::Trash,
        CategoryId::IosBackups | CategoryId::XcodeArchives | CategoryId::MailDownloads | CategoryId::Duplicates | CategoryId::LargeFiles => Mode::Trash,
        CategoryId::BrowserPrivacy => Mode::Trash,
    }
}

/// 有没有叫这个名字的进程在运行。
// ponytail: 每个条目查一次进程表，浏览器隐私最多十几项，不缓存
fn process_running(name: &str) -> bool {
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, sysinfo::ProcessRefreshKind::nothing());
    sys.processes().values().any(|p| p.name() == name)
}

/// 守卫：通过才允许动这个条目。返回 Err 时带上给用户看的原因。
fn guard(cat: &Category, item: &Item) -> Result<(), String> {
    if item.path == cat.root {
        return Err(t("不能删除扫描位置本身").into());
    }
    if !cat.only.is_empty() {
        // 固定路径的类别：条目必须就是规则里写明的某个目录，是真目录而不是链接，解析后仍在扫描位置之内
        let listed = cat.only.iter().any(|rel| cat.root.join(rel) == item.path);
        // 项目里的东西只认那几种依赖目录的名字，别的一概不碰
        let listed = listed && (cat.id != CategoryId::StaleProjects || crate::scan::PROJECT_DEP_DIRS.iter().any(|(d, _)| item.path.file_name() == Some(std::ffi::OsStr::new(d))));
        let meta = item.path.symlink_metadata().map_err(|e| format!("{}{e}", t("条目已不存在：")))?;
        let root = fs::canonicalize(&cat.root).map_err(|e| format!("{}{e}", t("扫描位置已不存在：")))?;
        let real = fs::canonicalize(&item.path).map_err(|e| format!("{}{e}", t("条目已不存在：")))?;
        // 对话记录有的是单个文件（一次会话一个 .jsonl）；大文件和重复文件只认普通文件；其余类别只认目录
        let kind_ok = match cat.id {
            CategoryId::AiHistory => meta.is_dir() || meta.is_file(),
            CategoryId::Duplicates | CategoryId::LargeFiles | CategoryId::BrowserPrivacy => meta.is_file(),
            _ => meta.is_dir(),
        };
        // 浏览器开着时挪走它的记录文件，它会继续往已挪走的文件里写，下次打开又是旧记录
        if cat.id == CategoryId::BrowserPrivacy && listed && process_running(crate::scan::browser_of(&item.path)) {
            return Err(t("先退出 {0} 再清理").replace("{0}", crate::scan::browser_of(&item.path)));
        }
        return if listed && kind_ok && real != root && real.starts_with(&root) {
            Ok(())
        } else {
            Err(t("路径不在允许删除的范围内").into())
        };
    }
    let (Some(parent), Some(name)) = (item.path.parent(), item.path.file_name()) else {
        return Err(t("路径不合法").into());
    };
    // 把上级目录里的符号链接和 .. 全部解析掉之后，条目必须仍然直接位于扫描位置之内
    let parent = fs::canonicalize(parent).map_err(|e| format!("{}{e}", t("条目已不存在：")))?;
    let root = fs::canonicalize(&cat.root).map_err(|e| format!("{}{e}", t("扫描位置已不存在：")))?;
    if parent != root || name == ".." || name == "." {
        return Err(t("路径不在允许删除的范围内").into());
    }
    let meta = item.path.symlink_metadata().map_err(|e| format!("{}{e}", t("条目已不存在：")))?;
    // 崩溃转储以 root 身份删，只放行 core.<数字> 普通文件，符号链接和目录都不行
    if cat.id == CategoryId::CrashDumps && !(meta.is_file() && name.to_str().is_some_and(crate::admin::is_core_dump_name)) {
        return Err(t("路径不在允许删除的范围内").into());
    }
    Ok(())
}

fn delete(path: &Path) -> Result<(), String> {
    let meta = path.symlink_metadata().map_err(|e| e.to_string())?;
    // 条目本身是符号链接时只删链接；remove_dir_all 也不会跟进目录里的符号链接
    if !meta.is_dir() {
        return fs::remove_file(path).map_err(|e| e.to_string());
    }
    fs::remove_dir_all(path)
        .or_else(|e| {
            // Go 模块缓存里的目录是只读的，删不了里面的文件：给目录补上写权限再删一次
            if e.kind() != std::io::ErrorKind::PermissionDenied {
                return Err(e);
            }
            make_dirs_writable(path)?;
            fs::remove_dir_all(path)
        })
        .map_err(|e| e.to_string())
}

/// 给 `dir` 和它下面的所有真目录加上属主的读写执行权限，不跟进符号链接。
fn make_dirs_writable(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::symlink_metadata(dir)?.permissions();
    perm.set_mode(perm.mode() | 0o700);
    fs::set_permissions(dir, perm)?;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            make_dirs_writable(&entry.path())?;
        }
    }
    Ok(())
}

/// 用系统接口把文件移到废纸篓。不经过「访达」，所以不需要自动化权限。
pub(crate) fn trash_real(path: &Path) -> Result<(), String> {
    use trash::macos::{DeleteMethod, TrashContextExtMacos};
    let mut ctx = trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(path).map_err(|e| e.to_string())
}

/// 用系统接口把文件移到废纸篓，返回它在废纸篓里的新位置（同名时系统会改名）。和 [`trash_real`] 是同一个
/// 系统调用（NSFileManager 的 trashItemAtURL），只是把新位置拿了回来，清理后才能「放回原处」。
pub(crate) fn trash_to(path: &Path) -> Result<PathBuf, String> {
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use std::ffi::{c_char, CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;
    let raw = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let is_dir = path.symlink_metadata().is_ok_and(|m| m.is_dir());
    let text = |s: *mut AnyObject| -> String {
        // SAFETY: s 是 NSString 或空指针；UTF8String 返回的指针在自动释放池里有效，这里立即复制出来
        unsafe {
            let p: *const c_char = if s.is_null() { std::ptr::null() } else { objc2::msg_send![s, UTF8String] };
            if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
        }
    };
    objc2::rc::autoreleasepool(|_| {
        let (Some(url_class), Some(fm_class)) = (AnyClass::get(c"NSURL"), AnyClass::get(c"NSFileManager")) else {
            return Err(t("移到废纸篓失败").to_owned());
        };
        // SAFETY: 都是 Foundation 公开的方法，签名与这里一致。路径按文件系统的原始字节传，不经过 UTF-8；
        // 两个输出参数是本函数里的局部指针，返回的对象都是自动释放的，用完前不会被释放。
        unsafe {
            let null: *mut AnyObject = std::ptr::null_mut();
            let url: *mut AnyObject = objc2::msg_send![url_class, fileURLWithFileSystemRepresentation: raw.as_ptr(), isDirectory: Bool::new(is_dir), relativeToURL: null];
            if url.is_null() {
                return Err(t("移到废纸篓失败").to_owned());
            }
            let fm: *mut AnyObject = objc2::msg_send![fm_class, defaultManager];
            let (mut moved, mut err): (*mut AnyObject, *mut AnyObject) = (null, null);
            let ok: Bool = objc2::msg_send![fm, trashItemAtURL: url, resultingItemURL: &mut moved, error: &mut err];
            if !ok.as_bool() || moved.is_null() {
                let desc: *mut AnyObject = if err.is_null() { null } else { objc2::msg_send![err, localizedDescription] };
                let msg = text(desc);
                return Err(if msg.is_empty() { t("移到废纸篓失败").to_owned() } else { msg });
            }
            let repr: *const c_char = objc2::msg_send![moved, fileSystemRepresentation];
            Ok(PathBuf::from(OsStr::from_bytes(CStr::from_ptr(repr).to_bytes())))
        }
    })
}

/// 把最近一次清理移到废纸篓的一项放回原处。`rec` 必须来自清理时记下的那份名单（由调用方保证），
/// 而且现在仍直接在 `trash_dir` 里；原位置不能已经有东西，上级文件夹还得在。放回用不覆盖的改名，不会盖掉任何东西。
pub fn put_back(rec: &Trashed, trash_dir: &Path) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    rec.to.symlink_metadata().map_err(|e| format!("{}{e}", t("废纸篓里找不到它：")))?;
    let in_trash = rec.to.parent().and_then(|p| fs::canonicalize(p).ok()).is_some_and(|p| Some(p) == fs::canonicalize(trash_dir).ok());
    if !in_trash {
        return Err(t("它已经不在废纸篓里了").into());
    }
    if rec.from.symlink_metadata().is_ok() {
        return Err(t("原来的位置已经有同名的东西了").into());
    }
    if !rec.from.parent().is_some_and(|p| p.symlink_metadata().is_ok_and(|m| m.is_dir())) {
        return Err(t("原来所在的文件夹已经不在了").into());
    }
    let (from, to) = (std::ffi::CString::new(rec.to.as_os_str().as_bytes()), std::ffi::CString::new(rec.from.as_os_str().as_bytes()));
    let (Ok(from), Ok(to)) = (from, to) else { return Err(t("路径不合法").into()) };
    // SAFETY: 两个都是以 0 结尾的路径；RENAME_EXCL：目标已存在时失败而不是覆盖
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

/// 扫描时记下的每个条目的 (设备号, inode)，按条目编号存。删除前再看一次，对不上就说明已经不是扫描时那个文件了。
/// 只看身份不看修改时间：缓存目录的修改时间时时在变，按它判断会把正常的缓存都当成「变了」。
pub type Stamps = HashMap<String, (u64, u64)>;

pub fn stamps(cats: &[Category]) -> Stamps {
    cats.iter().flat_map(|c| &c.items).filter_map(|i| i.path.symlink_metadata().ok().map(|m| (i.id.clone(), (m.dev(), m.ino())))).collect()
}

fn unchanged(item: &Item, stamps: &Stamps) -> Result<(), String> {
    let Some(&stamp) = stamps.get(&item.id) else { return Ok(()) };
    let meta = item.path.symlink_metadata().map_err(|e| format!("{}{e}", t("条目已不存在：")))?;
    if (meta.dev(), meta.ino()) == stamp {
        Ok(())
    } else {
        Err(t("扫描之后这里换成了别的文件，请重新扫描").into())
    }
}

/// 删除时用到的外部能力。正式运行时见 [`remove`]，测试里换成假的。
struct Env<'a> {
    /// 移到废纸篓，返回在废纸篓里的新位置
    trash: &'a dyn Fn(&Path) -> Result<PathBuf, String>,
    /// 删除需要管理员权限的那一批路径（正式运行时会弹系统密码框），一次调用处理全部
    as_admin: &'a dyn Fn(&[PathBuf]) -> Result<(), AdminError>,
    progress: &'a dyn Fn(CleanProgress),
    /// 置为 true 后，处理完手上这一项就停下
    cancel: &'a AtomicBool,
}

/// 删除 `ids` 指定的条目。`dry_run` 为 true 时走完全部守卫和统计，但不动任何文件。
/// 每处理一项调一次 `progress`；`cancel` 被置上后在两项之间停下，剩下的都不动。
pub fn remove(ids: &[String], report: &ScanReport, stamps: &Stamps, dry_run: bool, progress: &dyn Fn(CleanProgress), cancel: &AtomicBool) -> RemoveResult {
    remove_full(ids, report, stamps, dry_run, &Env { trash: &trash_to, as_admin: &crate::admin::remove_as_admin, progress, cancel })
}

fn remove_full(ids: &[String], report: &ScanReport, stamps: &Stamps, dry_run: bool, env: &Env) -> RemoveResult {
    let mut out = RemoveResult::default();
    // 需要管理员的条目先攒着，最后一起交出去，这样只弹一次密码框
    let mut privileged: Vec<&Item> = Vec::new();
    let total = ids.len() as u32;
    let find = |id: &String| report.categories.iter().find_map(|c| c.items.iter().find(|i| &i.id == id).map(|i| (c, i)));
    let stopped = |id: &String| Failure { path: find(id).map_or_else(|| id.into(), |(_, i)| i.path.clone()), reason: t("已停止").into() };
    for (n, id) in ids.iter().enumerate() {
        (env.progress)(CleanProgress { done: n as u32, total, admin: false });
        if env.cancel.load(Ordering::SeqCst) {
            out.failed.extend(ids[n..].iter().map(stopped));
            break;
        }
        let Some((cat, item)) = find(id) else {
            out.failed.push(Failure { path: id.into(), reason: t("不在扫描结果里").into() });
            continue;
        };
        let mode = mode_of(cat.id);
        let done = guard(cat, item).and_then(|()| unchanged(item, stamps)).and_then(|()| match (dry_run, cat.needs_admin, mode) {
            (true, _, _) => Ok(true),
            (false, true, _) => {
                privileged.push(item);
                Ok(false)
            }
            (false, false, Mode::Permanent) => delete(&item.path).map(|()| true),
            (false, false, Mode::Trash) => (env.trash)(&item.path).map(|to| {
                out.trashed.push(Trashed { from: item.path.clone(), to, size_bytes: item.size_bytes });
                true
            }),
        });
        match (done, mode) {
            (Ok(false), _) => {}
            (Ok(true), Mode::Permanent) => {
                out.freed_bytes += item.size_bytes;
                // pnpm、uv 等用 APFS 克隆导入文件，和项目里的副本共用数据块，实际释放的可能比量到的少
                out.estimated |= cat.id == CategoryId::DevCache;
            }
            (Ok(true), Mode::Trash) => out.trashed_bytes += item.size_bytes,
            (Err(reason), _) => out.failed.push(Failure { path: item.path.clone(), reason }),
        }
    }
    if env.cancel.load(Ordering::SeqCst) {
        // 停下了就不再弹密码框
        out.failed.extend(privileged.iter().map(|i| Failure { path: i.path.clone(), reason: t("已停止").into() }));
        privileged.clear();
    }
    if !privileged.is_empty() {
        (env.progress)(CleanProgress { done: total - privileged.len() as u32, total, admin: true });
        let paths: Vec<PathBuf> = privileged.iter().map(|i| i.path.clone()).collect();
        match (env.as_admin)(&paths) {
            Err(AdminError::Cancelled) => out.failed.extend(privileged.iter().map(|i| Failure { path: i.path.clone(), reason: t("已取消输入密码").into() })),
            // 命令报错时也可能已经删掉了大半（比如有一项正被系统占用），同样以实际结果为准
            done => {
                let reason = match done {
                    Err(AdminError::Failed(m)) if !m.is_empty() => m,
                    _ => t("没能删除").to_owned(),
                };
                for item in privileged {
                    // 以实际结果为准：还在的算失败
                    if item.path.symlink_metadata().is_err() {
                        out.freed_bytes += item.size_bytes;
                    } else {
                        out.failed.push(Failure { path: item.path.clone(), reason: reason.clone() });
                    }
                }
            }
        }
    }
    (env.progress)(CleanProgress { done: total, total, admin: false });
    out
}

/// 清理之后把已经不在磁盘上的条目从报告里去掉、重算合计，这样不用整份重新扫描。
/// 只看实际结果：删了一半、删失败的还在磁盘上，就留在报告里。同一路径出现在两个类别里时一起去掉。
pub fn prune(report: &mut ScanReport, ids: &[String]) {
    let asked: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let paths: HashSet<PathBuf> = report.categories.iter().flat_map(|c| &c.items).filter(|i| asked.contains(i.id.as_str())).map(|i| i.path.clone()).collect();
    for c in &mut report.categories {
        let before = c.items.len();
        c.items.retain(|i| !paths.contains(&i.path) || i.path.symlink_metadata().is_ok());
        if c.items.len() != before {
            c.total_bytes = c.items.iter().map(|i| i.size_bytes).sum();
        }
    }
}

// ---- 开发环境（规格切片 17） ----

/// 开发环境的清理目标。每个目标对应一条写死的命令，界面只能传种类和模拟器编号。
#[derive(serde::Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Target {
    DockerBuildCache,
    DockerStoppedContainers,
    DockerUnusedImages,
    SimUnavailable,
    SimDevice(String),
    SimRuntime(String),
}

/// 清理一次最多等这么久：删大镜像要时间
const DEVENV_TIMEOUT: Duration = Duration::from_secs(300);

pub type Runner<'a> = &'a dyn Fn(&Path, &[&str], Duration) -> Result<String, String>;

/// `now` 是删除前刚重新列出的结果：编号和「有没有设备在用」都以它为准。
/// 不直接删 Docker 和模拟器的文件，只调它们自己的命令。每条命令开始前调一次 `progress`；
/// `cancel` 被置上后不再开始下一条（正在跑的那条不打断，免得 Docker 或模拟器停在一半）。
pub fn clean_devenv(targets: &[Target], now: &crate::devenv::DevEnvReport, docker: Option<&Path>, dry_run: bool, run: Runner, progress: &dyn Fn(CleanProgress), cancel: &AtomicBool) -> RemoveResult {
    let mut targets = targets.to_vec();
    // 先删已停止的容器，它们占着的镜像才删得掉
    targets.sort_by_key(|t| match t {
        Target::DockerStoppedContainers => 0,
        Target::DockerUnusedImages => 1,
        Target::DockerBuildCache => 2,
        _ => 3,
    });
    let mut out = RemoveResult::default();
    let total = targets.len() as u32;
    for (n, target) in targets.iter().enumerate() {
        progress(CleanProgress { done: n as u32, total, admin: false });
        if cancel.load(Ordering::SeqCst) {
            out.failed.extend(targets[n..].iter().map(|x| Failure { path: PathBuf::from(format!("{x:?}")), reason: t("已停止").into() }));
            break;
        }
        let done = command_for(target, now, docker).and_then(|(bin, args)| {
            if dry_run {
                return Ok(());
            }
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            run(&bin, &args, DEVENV_TIMEOUT).map(|_| ())
        });
        if let Err(reason) = done {
            out.failed.push(Failure { path: PathBuf::from(format!("{target:?}")), reason });
        }
    }
    out
}

fn command_for(target: &Target, now: &crate::devenv::DevEnvReport, docker: Option<&Path>) -> Result<(PathBuf, Vec<String>), String> {
    let xcrun = PathBuf::from(crate::devenv::XCRUN);
    let docker = || docker.map(Path::to_path_buf).ok_or_else(|| t("没有找到 Docker").to_owned());
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let sims = now.simulators.as_ref();
    match target {
        Target::DockerBuildCache => Ok((docker()?, args(&["builder", "prune", "-a", "-f"]))),
        Target::DockerStoppedContainers => Ok((docker()?, args(&["container", "prune", "-f"]))),
        Target::DockerUnusedImages => Ok((docker()?, args(&["image", "prune", "-a", "-f"]))),
        Target::SimUnavailable => Ok((xcrun, args(&["simctl", "delete", "unavailable"]))),
        Target::SimDevice(udid) => {
            let known = is_uuid(udid) && sims.is_some_and(|s| s.devices.iter().any(|d| &d.udid == udid));
            if !known {
                return Err(t("这台模拟器已经不在了").into());
            }
            Ok((xcrun, args(&["simctl", "delete", udid])))
        }
        Target::SimRuntime(id) => {
            let runtime = sims.and_then(|s| s.runtimes.iter().find(|r| &r.id == id)).filter(|_| is_uuid(id)).ok_or_else(|| t("这个模拟器系统版本已经不在了").to_owned())?;
            if runtime.in_use {
                return Err(t("还有模拟器在用这个系统版本").into());
            }
            Ok((xcrun, args(&["simctl", "runtime", "delete", id])))
        }
    }
}

/// `5C0983CC-6196-45A4-ABD3-5DCE776893EC` 这样的格式
fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.char_indices().all(|(i, c)| if [8, 13, 18, 23].contains(&i) { c == '-' } else { c.is_ascii_hexdigit() })
}

/// 卸载一个应用：把应用本身和它的残留文件都移到废纸篓（规格 D-12：应用一律进废纸篓）。
/// 用户资源库里的启动代理先从系统里卸下来，免得它在登录期间还被反复拉起。
pub fn uninstall(app: &AppInfo, roots: &Roots, dry_run: bool) -> RemoveResult {
    let agents = roots.library.join("LaunchAgents");
    uninstall_with(app, roots, dry_run, &|path| {
        if path.parent() == Some(agents.as_path()) {
            // SAFETY: getuid 只读当前进程的用户号，不会失败。
            let domain = format!("gui/{}", unsafe { libc::getuid() });
            let _ = std::process::Command::new("/bin/launchctl").arg("bootout").arg(domain).arg(path).output();
        }
        trash_real(path)
    })
}

/// 卸载的守卫。应用必须直接位于某个应用目录下；残留必须是按应用包重新算出来、而且不和别的已装应用共用的路径之一。
fn uninstall_targets(app: &AppInfo, roots: &Roots) -> Result<Vec<(std::path::PathBuf, u64)>, String> {
    if app.running {
        return Err(t("应用没有退出（可能有没保存的内容），请手动退出后再卸载").into());
    }
    let canon = |p: &Path| fs::canonicalize(p).ok();
    let parent = app.path.parent().and_then(canon).ok_or(t("应用已不存在"))?;
    let in_apps_dir = roots.apps.iter().filter_map(|r| canon(r)).any(|r| r == parent);
    let is_bundle = app.path.extension().is_some_and(|x| x == "app") && app.path.symlink_metadata().is_ok_and(|m| m.is_dir());
    if !in_apps_dir || !is_bundle {
        return Err(t("路径不在允许卸载的范围内").into());
    }
    // 列表可能是几分钟前的：重新读一遍所有已装的应用，期间新装了同标识的应用也不会误带走它的数据。
    // 应用包读不出来时只移走应用本身，不动任何数据
    let all = installed(roots);
    let allowed = all.iter().position(|b| b.path == app.path).map(|i| leftovers_of(roots, &all, i).0).unwrap_or_default();
    let library = canon(&roots.library);
    let mut targets = vec![(app.path.clone(), app.size_bytes)];
    for l in &app.leftovers {
        // 残留必须在候选名单里，并且它的上级目录解析后仍在资源库之内
        let inside = l.path.parent().and_then(canon).zip(library.as_ref()).is_some_and(|(p, lib)| p.starts_with(lib));
        if allowed.contains(&l.path) && inside && l.path.symlink_metadata().is_ok() {
            targets.push((l.path.clone(), l.size_bytes.unwrap_or(0)));
        }
    }
    Ok(targets)
}

fn uninstall_with(app: &AppInfo, roots: &Roots, dry_run: bool, trash: &dyn Fn(&Path) -> Result<(), String>) -> RemoveResult {
    let mut out = RemoveResult::default();
    let targets = match uninstall_targets(app, roots) {
        Ok(t) => t,
        Err(reason) => {
            out.failed.push(Failure { path: app.path.clone(), reason });
            return out;
        }
    };
    for (i, (path, size)) in targets.iter().enumerate() {
        match if dry_run { Ok(()) } else { trash(path) } {
            Ok(()) => out.trashed_bytes += size,
            Err(reason) => {
                out.failed.push(Failure { path: path.clone(), reason });
                // 应用本身都没移走，就不要去动它的数据了
                if i == 0 {
                    break;
                }
            }
        }
    }
    out
}

/// 把已删除应用留下的数据移到废纸篓（规格 D-10、D-12：默认不勾选、标「需确认」、只进废纸篓）。
/// 也用来清刚拖进废纸篓的应用留下的数据。
/// `current` 是刚刚重新找出来的名单（[`crate::apps::orphan_paths`]），前端传来的路径只认这里面有的。
pub fn trash_orphans(paths: &[String], current: &[PathBuf], roots: &Roots) -> RemoveResult {
    trash_orphans_with(paths, current, roots, &trash_real)
}

fn trash_orphans_with(paths: &[String], current: &[PathBuf], roots: &Roots, trash: &dyn Fn(&Path) -> Result<(), String>) -> RemoveResult {
    let mut out = RemoveResult::default();
    let library = fs::canonicalize(&roots.library).ok();
    let deadline = std::time::Instant::now() + crate::apps::LEFTOVER_BUDGET;
    for p in paths {
        let path = PathBuf::from(p);
        // 上级目录解析后仍要在资源库之内：防止找完之后被换成了指向别处的链接
        let inside = path.parent().and_then(|p| fs::canonicalize(p).ok()).zip(library.as_ref()).is_some_and(|(p, lib)| p.starts_with(lib));
        // 链接不跟进去；目录和文件（偏好设置的 plist）都可以
        if !current.contains(&path) || !inside || !path.symlink_metadata().is_ok_and(|m| !m.file_type().is_symlink()) {
            out.failed.push(Failure { path, reason: t("路径不在允许删除的范围内").into() });
            continue;
        }
        let dev = path.symlink_metadata().map(|m| std::os::unix::fs::MetadataExt::dev(&m)).unwrap_or(0);
        let size = crate::scan::disk_usage_until(&path, dev, Some(deadline)).0;
        match trash(&path) {
            Ok(()) => out.trashed_bytes += size,
            Err(reason) => out.failed.push(Failure { path, reason }),
        }
    }
    out
}

/// 把空间去向里的一项移到废纸篓（规格切片 18）。前端传的是显示用路径，只认 `report` 树里标了可移走的那一项；
/// `root` 是显示用路径在磁盘上的前缀，正式运行时是 `/`。个人文件只进废纸篓，不直接删。
pub fn trash_space(path: &str, report: &crate::space::SpaceReport, root: &Path, trash: &dyn Fn(&Path) -> Result<(), String>) -> RemoveResult {
    let real = root.join(path.trim_start_matches('/'));
    let mut out = RemoveResult::default();
    match space_guard(path, report, &real).and_then(|size| trash(&real).map(|()| size)) {
        Ok(size) => out.trashed_bytes = size,
        Err(reason) => out.failed.push(Failure { path: real, reason }),
    }
    out
}

/// 守卫：在统计结果里、标了可移走、还在原处，且上级目录解析掉符号链接后仍是原来的位置（统计之后被换成链接的不碰）。通过时返回大小
fn space_guard(path: &str, report: &crate::space::SpaceReport, real: &Path) -> Result<u64, String> {
    let node = crate::space::find(&report.root, path).ok_or_else(|| t("不在空间统计结果里").to_string())?;
    if !node.trashable {
        return Err(t("这一项不能在空间去向里删除").into());
    }
    real.symlink_metadata().map_err(|e| format!("{}{e}", t("条目已不存在：")))?;
    let parent = real.parent().ok_or_else(|| t("不能删除扫描位置本身").to_string())?;
    if fs::canonicalize(parent).ok().as_deref() != Some(parent) {
        return Err(t("位置已经变了，请重新统计").into());
    }
    Ok(node.size_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::testutil::TempTree;
    use std::path::PathBuf;

    fn item(cat: CategoryId, path: PathBuf, size: u64) -> Item {
        Item { id: format!("{}:{}", cat.as_str(), path.display()), path, size_bytes: size, last_used_ms: None, partial: false }
    }
    fn category(id: CategoryId, root: PathBuf, items: Vec<Item>) -> Category {
        Category { id, root, total_bytes: 0, items, default_selected: true, removal: mode_of(id), needs_admin: false, needs_full_disk_access: false, unreadable: false, partial: false, only: vec![] }
    }
    fn ids(c: &Category) -> Vec<String> {
        c.items.iter().map(|i| i.id.clone()).collect()
    }
    /// 假废纸篓：把文件挪进临时目录，绝不碰真实的 ~/.Trash
    fn fake_trash(dir: PathBuf) -> impl Fn(&Path) -> Result<(), String> {
        move |p| {
            fs::create_dir_all(&dir).unwrap();
            fs::rename(p, dir.join(p.file_name().unwrap())).map_err(|e| e.to_string())
        }
    }
    /// 测试用的删除：没有扫描指纹，没有工具在运行，不该走到管理员那一步。记下的废纸篓位置就是原路径
    fn remove_with(ids: &[String], report: &ScanReport, dry_run: bool, trash: &dyn Fn(&Path) -> Result<(), String>) -> RemoveResult {
        let trash = |p: &Path| trash(p).map(|()| p.to_path_buf());
        remove_full(ids, report, &Stamps::new(), dry_run, &Env { trash: &trash, as_admin: &|_| panic!("不该要管理员权限"), progress: &|_| {}, cancel: &AtomicBool::new(false) })
    }
    /// 测试用的删除：管理员那一步换成假的
    fn remove_admin(ids: &[String], report: &ScanReport, as_admin: &dyn Fn(&[PathBuf]) -> Result<(), AdminError>) -> RemoveResult {
        remove_full(ids, report, &Stamps::new(), false, &Env { trash: &|_| panic!("不该进废纸篓"), as_admin, progress: &|_| {}, cancel: &AtomicBool::new(false) })
    }
    fn no_progress(_: CleanProgress) {}

    fn sim_device(udid: &str, runtime: &str) -> crate::devenv::SimDevice {
        crate::devenv::SimDevice { udid: udid.into(), name: "iPhone".into(), runtime: runtime.into(), os: String::new(), available: true, size_bytes: 1, last_used_ms: None }
    }
    fn sim_runtime(id: &str, in_use: bool) -> crate::devenv::SimRuntime {
        crate::devenv::SimRuntime { id: id.into(), runtime: String::new(), os: String::new(), version: String::new(), size_bytes: 1, deletable: true, in_use }
    }
    const UDID: &str = "5C0983CC-6196-45A4-ABD3-5DCE776893EC";
    const FREE_RT: &str = "AAAAAAAA-0000-4000-8000-000000000001";
    const USED_RT: &str = "AAAAAAAA-0000-4000-8000-000000000002";
    fn devenv_now() -> crate::devenv::DevEnvReport {
        crate::devenv::DevEnvReport {
            docker: crate::devenv::Docker::NotRunning,
            simulators: Some(crate::devenv::Simulators { devices: vec![sim_device(UDID, "x")], runtimes: vec![sim_runtime(FREE_RT, false), sim_runtime(USED_RT, true)] }),
        }
    }

    #[test]
    fn 开发环境只执行写死的命令_先删容器再删镜像() {
        let calls = std::cell::RefCell::new(Vec::new());
        let run = |bin: &Path, args: &[&str], _: std::time::Duration| {
            calls.borrow_mut().push(format!("{} {}", bin.display(), args.join(" ")));
            Ok(String::new())
        };
        let targets = [Target::DockerBuildCache, Target::DockerUnusedImages, Target::DockerStoppedContainers, Target::SimUnavailable, Target::SimDevice(UDID.into()), Target::SimRuntime(FREE_RT.into())];
        let r = clean_devenv(&targets, &devenv_now(), Some(Path::new("/usr/local/bin/docker")), false, &run, &no_progress, &AtomicBool::new(false));
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(
            *calls.borrow(),
            [
                "/usr/local/bin/docker container prune -f".to_owned(),
                "/usr/local/bin/docker image prune -a -f".into(),
                "/usr/local/bin/docker builder prune -a -f".into(),
                "/usr/bin/xcrun simctl delete unavailable".into(),
                format!("/usr/bin/xcrun simctl delete {UDID}"),
                format!("/usr/bin/xcrun simctl runtime delete {FREE_RT}"),
            ]
        );
        // 试运行：一条都不执行
        calls.borrow_mut().clear();
        assert!(clean_devenv(&targets, &devenv_now(), Some(Path::new("/usr/local/bin/docker")), true, &run, &no_progress, &AtomicBool::new(false)).failed.is_empty());
        assert!(calls.borrow().is_empty());
    }

    #[test]
    fn 开发环境拒绝结果之外的编号() {
        let calls = std::cell::RefCell::new(0);
        let run = |_: &Path, _: &[&str], _: std::time::Duration| {
            *calls.borrow_mut() += 1;
            Ok(String::new())
        };
        let targets = [
            Target::SimDevice("not-a-uuid".into()),
            Target::SimDevice("../../etc/passwd".into()),
            Target::SimDevice("0A1B2C3D-0000-4000-8000-000000000099".into()), // 格式对，但不在列表里
            Target::SimRuntime(USED_RT.into()),                                // 还有设备在用
            Target::DockerBuildCache,                                           // 没找到 docker
        ];
        let r = clean_devenv(&targets, &devenv_now(), None, false, &run, &no_progress, &AtomicBool::new(false));
        assert_eq!((*calls.borrow(), r.failed.len()), (0, 5));
    }

    #[test]
    fn 正常条目按类别直接删除或移到废纸篓() {
        let t = TempTree::new();
        let (a, b, d) = (t.file("logs/a.log", 10), t.file("logs/dir/b.log", 10), t.file("dl/old.zip", 10));
        let logs = category(CategoryId::Logs, t.0.join("logs"), vec![item(CategoryId::Logs, a.clone(), 100), item(CategoryId::Logs, t.0.join("logs/dir"), 200)]);
        let dl = category(CategoryId::OldDownloads, t.0.join("dl"), vec![item(CategoryId::OldDownloads, d.clone(), 50)]);
        let all: Vec<String> = ids(&logs).into_iter().chain(ids(&dl)).collect();
        let report = ScanReport { categories: vec![logs, dl], ..Default::default() };
        let r = remove_with(&all, &report, false, &fake_trash(t.0.join("trash")));
        assert_eq!((r.freed_bytes, r.trashed_bytes, r.failed.len()), (300, 50, 0));
        assert_eq!(r.trashed, [Trashed { from: d.clone(), to: d.clone(), size_bytes: 50 }], "记下了移进废纸篓的是哪一项");
        assert!(!a.exists() && !b.exists() && !d.exists());
        assert!(t.0.join("trash/old.zip").exists(), "个人文件进了废纸篓而不是被删掉");
        assert!(t.0.join("logs").exists(), "扫描位置本身还在");
    }

    #[test]
    fn 演练模式不动任何文件但统计一致() {
        let t = TempTree::new();
        let a = t.file("logs/a.log", 10);
        let logs = category(CategoryId::Logs, t.0.join("logs"), vec![item(CategoryId::Logs, a.clone(), 100)]);
        let all = ids(&logs);
        let r = remove_with(&all, &ScanReport { categories: vec![logs], ..Default::default() }, true, &|_| panic!("演练不该动废纸篓"));
        assert_eq!(r.freed_bytes, 100);
        assert!(a.exists());
    }

    #[test]
    fn 拒绝不在报告里的编号和跳出扫描位置的路径() {
        let t = TempTree::new();
        let secret = t.file("home/secret.txt", 10);
        t.file("logs/a.log", 10);
        let escape = item(CategoryId::Logs, t.0.join("logs/../home/secret.txt"), 1);
        let nested = item(CategoryId::Logs, t.0.join("logs/../home"), 1);
        let root_itself = item(CategoryId::Logs, t.0.join("logs"), 1);
        let logs = category(CategoryId::Logs, t.0.join("logs"), vec![escape, nested, root_itself]);
        let mut all = ids(&logs);
        all.push("logs:/etc/hosts".into());
        let r = remove_with(&all, &ScanReport { categories: vec![logs], ..Default::default() }, false, &|_| Ok(()));
        assert_eq!((r.freed_bytes, r.failed.len()), (0, 4));
        assert!(secret.exists() && t.0.join("logs/a.log").exists());
    }

    #[test]
    fn 条目是符号链接时只删链接() {
        let t = TempTree::new();
        let outside = t.file("outside/keep.txt", 10);
        t.file("logs/a.log", 10);
        let link = t.0.join("logs/link");
        std::os::unix::fs::symlink(t.0.join("outside"), &link).unwrap();
        let logs = category(CategoryId::Logs, t.0.join("logs"), vec![item(CategoryId::Logs, link.clone(), 0)]);
        let all = ids(&logs);
        let r = remove_with(&all, &ScanReport { categories: vec![logs], ..Default::default() }, false, &|_| Ok(()));
        assert!(r.failed.is_empty());
        assert!(link.symlink_metadata().is_err(), "链接没了");
        assert!(outside.exists(), "链接指向的内容还在");
    }

    #[test]
    fn 扫描位置被换成符号链接时拒绝删除() {
        let t = TempTree::new();
        let victim = t.file("real/victim.txt", 10);
        t.file("logs/x", 1);
        let it = item(CategoryId::Logs, t.0.join("logs/sub/victim.txt"), 1);
        // 扫描之后，logs/sub 被换成了指向别处的链接
        std::os::unix::fs::symlink(t.0.join("real"), t.0.join("logs/sub")).unwrap();
        let logs = category(CategoryId::Logs, t.0.join("logs"), vec![it]);
        let all = ids(&logs);
        let r = remove_with(&all, &ScanReport { categories: vec![logs], ..Default::default() }, false, &|_| Ok(()));
        assert_eq!(r.failed.len(), 1);
        assert!(victim.exists());
    }

    fn listed(roots: &Roots) -> AppInfo {
        let mut app = crate::apps::list(roots, &|_| Default::default(), &|_| (false, vec![])).remove(0);
        crate::apps::measure(&mut app, std::time::Duration::from_secs(5));
        app
    }

    #[test]
    fn 卸载把应用和残留一起移到废纸篓() {
        let t = TempTree::new();
        let (app_path, roots) = crate::apps::testutil::fake_app(&t, "Demo", "com.example.demo");
        let cache = t.file("Library/Caches/com.example.demo/c.bin", 50_000);
        let other = t.file("Library/Caches/com.example.demo2/c.bin", 10);
        let app = listed(&roots);

        let dry = uninstall_with(&app, &roots, true, &|_| panic!("演练不该动废纸篓"));
        assert!(dry.failed.is_empty() && dry.trashed_bytes == app.size_bytes + app.leftover_bytes.unwrap() && app_path.exists());

        let r = uninstall_with(&app, &roots, false, &fake_trash(t.0.join("trash")));
        assert_eq!((r.freed_bytes, r.trashed_bytes, r.failed.len()), (0, dry.trashed_bytes, 0));
        assert!(!app_path.exists() && !cache.exists() && other.exists());
        assert!(t.0.join("trash/Demo.app").exists() && t.0.join("trash/com.example.demo").exists());
    }

    #[test]
    fn 卸载的守卫() {
        let t = TempTree::new();
        let (app_path, roots) = crate::apps::testutil::fake_app(&t, "Demo", "com.example.demo");
        let secret = t.file("Library/Keychains/login.keychain", 10);
        let outside = t.file("elsewhere/Fake.app/Contents/x", 10);
        let app = listed(&roots);
        let never = |_: &Path| -> Result<(), String> { panic!("被拒绝的卸载不该动任何文件") };

        let running = AppInfo { running: true, ..app.clone() };
        assert_eq!(uninstall_with(&running, &roots, false, &never).failed[0].reason, "应用没有退出（可能有没保存的内容），请手动退出后再卸载");

        let moved = AppInfo { path: outside.parent().unwrap().parent().unwrap().to_path_buf(), ..app.clone() };
        assert_eq!(uninstall_with(&moved, &roots, false, &never).failed[0].reason, "路径不在允许卸载的范围内");

        let not_bundle = AppInfo { path: roots.apps[0].clone(), ..app.clone() };
        assert_eq!(uninstall_with(&not_bundle, &roots, false, &never).failed.len(), 1);

        // 残留列表被塞进了不属于它的路径：只移走应用，那条路径不碰
        let tampered = AppInfo { leftovers: vec![crate::model::Leftover { path: secret.clone(), size_bytes: Some(10) }], ..app.clone() };
        let r = uninstall_with(&tampered, &roots, false, &fake_trash(t.0.join("trash")));
        assert!(r.failed.is_empty() && secret.exists() && !app_path.exists());
    }

    #[test]
    fn 应用本身移不走时不动它的数据() {
        let t = TempTree::new();
        let (app_path, roots) = crate::apps::testutil::fake_app(&t, "Demo", "com.example.demo");
        let cache = t.file("Library/Caches/com.example.demo/c.bin", 10);
        let r = uninstall_with(&listed(&roots), &roots, false, &|_| Err("没有权限".into()));
        assert_eq!((r.trashed_bytes, r.failed.len()), (0, 1));
        assert!(app_path.exists() && cache.exists());
    }

    #[test]
    fn 卸载不带走和别的已装应用共用的数据() {
        let t = TempTree::new();
        let (old, roots) = crate::apps::testutil::fake_app(&t, "starKnowledge", "com.example.km");
        let shared = t.file("Library/Application Support/com.example.km/db", 10);
        let own = t.file("Library/Application Support/starKnowledge/x", 10);
        // 列出时只装着一份，所以共用目录被算作它的残留
        let app = listed(&roots);
        assert_eq!(app.leftovers.len(), 2);
        // 卸载前又装上了同标识的另一份：动手前重新查，共用的那份不动
        let (new, _) = crate::apps::testutil::fake_app(&t, "星识", "com.example.km");
        let r = uninstall_with(&app, &roots, false, &fake_trash(t.0.join("trash")));
        assert!(r.failed.is_empty() && !old.exists() && !own.exists());
        assert!(shared.exists() && new.exists());
        // 两份都在时，列表里就标明了共用
        crate::apps::testutil::fake_app(&t, "starKnowledge", "com.example.km");
        let apps = crate::apps::list(&roots, &|_| Default::default(), &|_| (false, vec![]));
        let a = apps.iter().find(|a| a.name == "starKnowledge").unwrap();
        assert!(a.leftovers.is_empty() && a.kept.len() == 1 && a.kept[0].shared_with.as_deref() == Some("星识"));
        uninstall_with(a, &roots, false, &fake_trash(t.0.join("trash2")));
        assert!(shared.exists());
    }

    #[test]
    fn 卸载不动要管理员权限的东西() {
        let t = TempTree::new();
        let (app_path, roots) = crate::apps::testutil::fake_app(&t, "Demo", "com.example.demo");
        let daemon = t.0.join("SystemLibrary/LaunchDaemons/com.example.demo.plist");
        fs::create_dir_all(daemon.parent().unwrap()).unwrap();
        let program = app_path.join("Contents/MacOS/daemon").display().to_string();
        plist::Value::Dictionary([("Program".to_owned(), plist::Value::String(program))].into_iter().collect()).to_file_xml(&daemon).unwrap();
        let app = listed(&roots);
        assert_eq!(app.kept.iter().map(|k| (&k.path, &k.shared_with)).collect::<Vec<_>>(), [(&daemon, &None)]);
        let r = uninstall_with(&app, &roots, false, &fake_trash(t.0.join("trash")));
        assert!(r.failed.is_empty() && !app_path.exists() && daemon.exists());
    }

    #[test]
    fn 已删除应用的残留只移走重新找出来的() {
        let t = TempTree::new();
        let (_, roots) = crate::apps::testutil::fake_app(&t, "Demo", "com.example.demo");
        let gone = t.file("Library/Containers/com.gone.app/Data/x", 10).parent().unwrap().parent().unwrap().to_path_buf();
        let keep = t.file("Library/Containers/com.example.demo/Data/x", 10).parent().unwrap().parent().unwrap().to_path_buf();
        let secret = t.file("Library/Keychains/login.keychain", 10);
        let current = vec![gone.clone()];
        let ask: Vec<String> = [&gone, &keep, &secret].iter().map(|p| p.display().to_string()).collect();
        let r = trash_orphans_with(&ask, &current, &roots, &fake_trash(t.0.join("trash")));
        assert!(r.trashed_bytes > 0 && r.failed.len() == 2);
        assert!(!gone.exists() && keep.exists() && secret.exists());
    }

    #[test]
    fn 崩溃转储只放行扫描位置下直接的_core_数字_普通文件() {
        let t = TempTree::new();
        let ok = t.file("cores/core.123", 10);
        let (dir, link, other, nested) = (t.0.join("cores/core.5"), t.0.join("cores/core.6"), t.file("cores/core.x", 10), t.file("cores/sub/core.7", 10));
        fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(t.file("elsewhere/core.8", 10), &link).unwrap();
        let escape = t.0.join("cores/../elsewhere/core.8");
        let paths = [ok.clone(), dir, link, other, nested, escape];
        let mut cat = category(CategoryId::CrashDumps, t.0.join("cores"), paths.iter().map(|p| item(CategoryId::CrashDumps, p.clone(), 10)).collect());
        cat.needs_admin = true;
        let all = ids(&cat);
        let report = ScanReport { categories: vec![cat], ..Default::default() };
        let asked = std::cell::RefCell::new(Vec::new());
        let admin = |p: &[PathBuf]| {
            asked.borrow_mut().extend_from_slice(p);
            p.iter().try_for_each(fs::remove_file).map_err(|e| AdminError::Failed(e.to_string()))
        };
        let r = remove_admin(&all, &report, &admin);
        assert_eq!(*asked.borrow(), vec![ok.clone()], "只有它交给管理员删");
        assert_eq!((r.freed_bytes, r.failed.len()), (10, 5), "目录、符号链接、名字不对、在子目录里、经 .. 跑出去的都拒绝");
        assert!(!ok.exists() && t.0.join("elsewhere/core.8").exists());
    }

    #[test]
    fn 需要管理员的条目攒在一起只交出去一次() {
        let t = TempTree::new();
        let (sys1, sys2, ok) = (t.file("sys/c1", 10), t.file("sys/c2", 10), t.file("logs/a.log", 10));
        let mut system = category(CategoryId::SystemCache, t.0.join("sys"), vec![item(CategoryId::SystemCache, sys1.clone(), 10), item(CategoryId::SystemCache, sys2.clone(), 20)]);
        system.needs_admin = true;
        let logs = category(CategoryId::Logs, t.0.join("logs"), vec![item(CategoryId::Logs, t.0.join("logs/gone"), 5), item(CategoryId::Logs, ok.clone(), 7)]);
        let all: Vec<String> = ids(&system).into_iter().chain(ids(&logs)).collect();
        let report = ScanReport { categories: vec![system, logs], ..Default::default() };
        let calls = std::cell::RefCell::new(Vec::new());
        // 假的「管理员」：只真的删掉第一个，第二个留着，模拟部分成功
        let admin = |paths: &[PathBuf]| {
            calls.borrow_mut().push(paths.to_vec());
            fs::remove_file(&paths[0]).map_err(|e| AdminError::Failed(e.to_string()))
        };
        let r = remove_admin(&all, &report, &admin);
        assert_eq!(*calls.borrow(), vec![vec![sys1.clone(), sys2.clone()]]);
        assert_eq!(r.freed_bytes, 10 + 7);
        assert_eq!(r.failed.len(), 2, "没删掉的系统缓存和已经不存在的日志");
        assert!(!sys1.exists() && sys2.exists() && !ok.exists());

        // 用户取消输密码：这批全部算失败，别的照常
        let t2 = TempTree::new();
        let s = t2.file("sys/c", 10);
        let mut system = category(CategoryId::SystemCache, t2.0.join("sys"), vec![item(CategoryId::SystemCache, s.clone(), 10)]);
        system.needs_admin = true;
        let all = ids(&system);
        let r = remove_admin(&all, &ScanReport { categories: vec![system], ..Default::default() }, &|_| Err(AdminError::Cancelled));
        assert_eq!((r.freed_bytes, r.failed[0].reason.as_str()), (0, "已取消输入密码"));
        assert!(s.exists());
    }

    #[test]
    fn 项目里只把认得的依赖目录移到废纸篓() {
        let t = TempTree::new();
        let (dep, src) = (t.file("code/p/node_modules/x/i.js", 10), t.file("code/p/src/a.js", 10));
        let mk = |rel: &str| item(CategoryId::StaleProjects, t.0.join(rel), 7);
        let mut cat = category(CategoryId::StaleProjects, t.0.clone(), vec![mk("code/p/node_modules"), mk("code/p/src"), mk("code/p")]);
        // 就算名单被塞进了别的目录，名字不是依赖目录的也不动
        cat.only = vec!["code/p/node_modules".into(), "code/p/src".into(), "code/p".into()];
        let all = ids(&cat);
        let r = remove_with(&all, &ScanReport { categories: vec![cat], ..Default::default() }, false, &fake_trash(t.0.join("trash")));
        assert_eq!((r.freed_bytes, r.trashed_bytes, r.failed.len()), (0, 7, 2));
        assert!(!dep.exists() && src.exists() && t.0.join("trash/node_modules/x/i.js").exists());
    }

    #[test]
    fn 固定路径类别的守卫() {
        let t = TempTree::new();
        let cache = t.file(".npm/_cacache/x", 10);
        let secret = t.file("Documents/secret", 10);
        let outside = t.file("../outside-pika/keep", 10);
        std::os::unix::fs::symlink(outside.parent().unwrap(), t.0.join(".gradle-link")).unwrap();
        let mk = |rel: &str| item(CategoryId::DevCache, t.0.join(rel), 5);
        let mut dev = category(CategoryId::DevCache, t.0.clone(), vec![mk(".npm/_cacache"), mk("Documents"), mk(".gradle-link"), mk(".npm/_cacache/../../Documents")]);
        dev.only = vec![".npm/_cacache".into(), ".gradle-link".into()];
        let all = ids(&dev);
        let r = remove_with(&all, &ScanReport { categories: vec![dev], ..Default::default() }, false, &|_| Ok(()));
        assert_eq!((r.freed_bytes, r.failed.len()), (5, 3));
        assert!(!cache.exists() && secret.exists() && outside.exists());
        let _ = fs::remove_dir_all(outside.parent().unwrap());
    }

    #[test]
    fn 只读目录也能删掉() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempTree::new();
        let f = t.file("go/pkg/mod/golang.org/x/net@v1/a.go", 10);
        fs::set_permissions(f.parent().unwrap(), fs::Permissions::from_mode(0o555)).unwrap();
        let mut dev = category(CategoryId::DevCache, t.0.clone(), vec![item(CategoryId::DevCache, t.0.join("go/pkg/mod"), 10)]);
        dev.only = vec!["go/pkg/mod".into()];
        let all = ids(&dev);
        let r = remove_with(&all, &ScanReport { categories: vec![dev], ..Default::default() }, false, &|_| Ok(()));
        assert_eq!((r.freed_bytes, r.failed.len()), (10, 0));
        assert!(!t.0.join("go/pkg/mod").exists());
        assert!(r.estimated, "开发工具缓存可能和项目共用数据块，释放量写「约」");
    }

    #[test]
    fn 重复文件和大文件只移走名单里的普通文件() {
        let t = TempTree::new();
        let dup = t.file("Downloads/a copy.mov", 10);
        let inside = t.file("Documents/folder/x", 10);
        for id in [CategoryId::Duplicates, CategoryId::LargeFiles] {
            assert_eq!(mode_of(id), Mode::Trash);
        }
        let mk = |rel: &str| item(CategoryId::Duplicates, t.0.join(rel), 5);
        let mut cat = category(CategoryId::Duplicates, t.0.clone(), vec![mk("Downloads/a copy.mov"), mk("Documents/folder")]);
        cat.only = vec!["Downloads/a copy.mov".into(), "Documents/folder".into()];
        let all = ids(&cat);
        let r = remove_with(&all, &ScanReport { categories: vec![cat], ..Default::default() }, false, &fake_trash(t.0.join("trash")));
        assert_eq!((r.freed_bytes, r.trashed_bytes, r.failed.len()), (0, 5, 1), "文件夹不行");
        assert!(!dup.exists() && inside.exists() && t.0.join("trash/a copy.mov").exists());
    }

    #[test]
    fn 对话记录可以移走单个会话文件但别的固定路径类别不行() {
        let t = TempTree::new();
        let session = t.file(".codex/archived_sessions/a.jsonl", 10);
        let cache_file = t.file(".codex/log/x.log", 10);
        let mut hist = category(CategoryId::AiHistory, t.0.clone(), vec![item(CategoryId::AiHistory, session.clone(), 4)]);
        hist.only = vec![".codex/archived_sessions/a.jsonl".into()];
        let mut cache = category(CategoryId::AiCache, t.0.clone(), vec![item(CategoryId::AiCache, cache_file.clone(), 3)]);
        cache.only = vec![".codex/log/x.log".into()];
        let all = [ids(&hist), ids(&cache)].concat();
        let r = remove_with(&all, &ScanReport { categories: vec![hist, cache], ..Default::default() }, false, &fake_trash(t.0.join("trash")));
        assert_eq!((r.trashed_bytes, r.failed.len()), (4, 1));
        assert!(!session.exists() && cache_file.exists());
    }

    /// 统计一棵临时的「个人文件夹」，个人文件夹在显示路径 `/Users/me`
    fn space_report(t: &TempTree) -> crate::space::SpaceReport {
        let limits = crate::space::Limits { top: 30, min_bytes: 1 };
        let mut r = crate::space::scan(&t.0, Path::new("/Users/me"), &[], limits, &std::sync::atomic::AtomicBool::new(false), &|_| {});
        crate::space::mark_trashable(&mut r.root, Path::new("/Users/me"));
        r
    }

    #[test]
    fn 空间去向_个人文件移到废纸篓() {
        let t = TempTree::new();
        let f = t.file("Users/me/proj/big", 4096);
        let r = space_report(&t);
        let size = crate::space::find(&r.root, "/Users/me/proj").unwrap().size_bytes;
        let out = trash_space("/Users/me/proj", &r, &t.0, &fake_trash(t.0.join("trash")));
        assert!(out.failed.is_empty(), "{:?}", out.failed);
        assert_eq!((out.trashed_bytes, out.freed_bytes), (size, 0), "只进废纸篓，不算已释放");
        assert!(!f.exists() && t.0.join("trash/proj/big").exists());
    }

    #[test]
    fn 空间去向_拒绝树外和不让删的路径() {
        let t = TempTree::new();
        t.file("Users/me/proj/big", 4096);
        t.file("Users/me/Library/Mail/big", 4096);
        t.file("opt/big", 4096);
        let r = space_report(&t);
        for p in ["/Users/me/../me/proj", "/Users/me/nope", "/etc", "/Users/me/Library", "/Users/me/Library/Mail", "/Users/me", "/opt", "/"] {
            let out = trash_space(p, &r, &t.0, &|_| panic!("{p} 不该走到删除"));
            assert_eq!(out.failed.len(), 1, "{p} 应该被拒绝");
            assert_eq!(out.trashed_bytes, 0);
        }
        assert!(t.0.join("Users/me/Library/Mail/big").exists());
    }

    #[test]
    fn 空间去向_统计后上级被换成链接就不删() {
        let t = TempTree::new();
        t.file("Users/me/proj/sub/big", 4096);
        let r = space_report(&t);
        // 统计之后 proj 被换成指向别处的链接，别处恰好也有 sub
        let elsewhere = t.file("elsewhere/sub/big", 4096);
        fs::rename(t.0.join("Users/me/proj"), t.0.join("moved")).unwrap();
        std::os::unix::fs::symlink(t.0.join("elsewhere"), t.0.join("Users/me/proj")).unwrap();
        let out = trash_space("/Users/me/proj/sub", &r, &t.0, &|_| panic!("不该走到删除"));
        assert_eq!(out.failed.len(), 1);
        assert!(elsewhere.exists());
    }

    #[test]
    fn 空间去向_已经不在了就报错() {
        let t = TempTree::new();
        t.file("Users/me/proj/big", 4096);
        let r = space_report(&t);
        fs::remove_dir_all(t.0.join("Users/me/proj")).unwrap();
        let out = trash_space("/Users/me/proj", &r, &t.0, &|_| panic!("不该走到删除"));
        assert_eq!(out.failed.len(), 1);
    }

    #[test]
    fn 管理员命令报错时按实际结果算() {
        let t = TempTree::new();
        let (s1, s2) = (t.file("sys/c1", 10), t.file("sys/c2", 10));
        let mut system = category(CategoryId::SystemCache, t.0.join("sys"), vec![item(CategoryId::SystemCache, s1.clone(), 10), item(CategoryId::SystemCache, s2.clone(), 20)]);
        system.needs_admin = true;
        let all = ids(&system);
        // 删掉了第一个，第二个正被占用，rm 整体返回失败
        let admin = |p: &[PathBuf]| {
            fs::remove_file(&p[0]).unwrap();
            Err(AdminError::Failed("rm: c2: Directory not empty".into()))
        };
        let r = remove_admin(&all, &ScanReport { categories: vec![system], ..Default::default() }, &admin);
        assert_eq!(r.freed_bytes, 10, "已经删掉的照样算已释放");
        assert_eq!((r.failed.len(), r.failed[0].path.clone(), r.failed[0].reason.as_str()), (1, s2.clone(), "rm: c2: Directory not empty"));
    }

    #[test]
    fn 扫描之后被换成别的文件就不删() {
        let t = TempTree::new();
        let (same, swapped) = (t.file("logs/a.log", 10), t.file("logs/b.log", 10));
        let logs = category(CategoryId::Logs, t.0.join("logs"), vec![item(CategoryId::Logs, same.clone(), 1), item(CategoryId::Logs, swapped.clone(), 1)]);
        let stamps = stamps(std::slice::from_ref(&logs));
        // 扫描之后 b.log 被删掉又出现了一个同名的新文件
        fs::remove_file(&swapped).unwrap();
        fs::write(&swapped, b"new").unwrap();
        let all = ids(&logs);
        let r = remove_full(&all, &ScanReport { categories: vec![logs], ..Default::default() }, &stamps, false, &Env { trash: &|_| panic!(), as_admin: &|_| panic!(), progress: &no_progress, cancel: &AtomicBool::new(false) });
        assert_eq!((r.freed_bytes, r.failed.len()), (1, 1));
        assert!(!same.exists() && swapped.exists());
    }

    #[test]
    fn 停止后剩下的条目都不动并报告进度() {
        let t = TempTree::new();
        let (a, b) = (t.file("logs/a.log", 10), t.file("logs/b.log", 10));
        let logs = category(CategoryId::Logs, t.0.join("logs"), vec![item(CategoryId::Logs, a.clone(), 1), item(CategoryId::Logs, b.clone(), 1)]);
        let all = ids(&logs);
        let cancel = AtomicBool::new(false);
        let seen = std::cell::RefCell::new(Vec::new());
        // 处理完第一项后用户点了「停止」
        let progress = |p: CleanProgress| {
            seen.borrow_mut().push(p.done);
            if p.done == 1 {
                cancel.store(true, Ordering::SeqCst);
            }
        };
        let r = remove_full(&all, &ScanReport { categories: vec![logs], ..Default::default() }, &Stamps::new(), false, &Env { trash: &|_| panic!(), as_admin: &|_| panic!(), progress: &progress, cancel: &cancel });
        assert_eq!((r.freed_bytes, r.failed.len(), r.failed[0].reason.as_str()), (1, 1, "已停止"));
        assert_eq!(*seen.borrow(), [0, 1, 2]);
        assert!(!a.exists() && b.exists());
    }

    #[test]
    fn 清理后从报告里去掉已经删掉的条目() {
        let t = TempTree::new();
        let (gone, kept, other) = (t.file("logs/a", 1), t.file("logs/b", 1), t.file("logs/c", 1));
        let logs = category(CategoryId::Logs, t.0.join("logs"), vec![item(CategoryId::Logs, gone.clone(), 5), item(CategoryId::Logs, kept.clone(), 7), item(CategoryId::Logs, other.clone(), 9)]);
        // 同一个路径也出现在另一类里
        let dup = category(CategoryId::AppCache, t.0.join("logs"), vec![item(CategoryId::AppCache, gone.clone(), 5)]);
        let asked = vec![logs.items[0].id.clone(), logs.items[1].id.clone()];
        let mut report = ScanReport { categories: vec![logs, dup], ..Default::default() };
        fs::remove_file(&gone).unwrap(); // kept 删失败了，还在
        prune(&mut report, &asked);
        assert_eq!(report.categories[0].items.iter().map(|i| i.size_bytes).collect::<Vec<_>>(), [7, 9]);
        assert_eq!((report.categories[0].total_bytes, report.categories[1].items.len()), (16, 0));
    }

    #[test]
    fn 放回原处的守卫() {
        let t = TempTree::new();
        let trash = t.0.join("Trash");
        let rec = |from: &str, to: &str| Trashed { from: t.0.join(from), to: t.0.join(to), size_bytes: 1 };
        t.file("Trash/node_modules 2/x", 1);
        t.file("p/keep", 1);
        // 正常：原位置空着，上级还在
        assert_eq!(put_back(&rec("p/node_modules", "Trash/node_modules 2"), &trash), Ok(()));
        assert!(t.0.join("p/node_modules/x").exists() && !t.0.join("Trash/node_modules 2").exists());
        // 原位置已经有东西：不覆盖
        t.file("Trash/keep", 1);
        assert!(put_back(&rec("p/keep", "Trash/keep"), &trash).is_err());
        // 不在废纸篓里的、原来的文件夹没了的都不动
        assert!(put_back(&rec("q/x", "p/keep"), &trash).is_err());
        assert!(put_back(&rec("gone/keep", "Trash/keep"), &trash).is_err());
        assert!(t.0.join("Trash/keep").exists() && t.0.join("p/keep").exists());
    }

    /// 真的移进本机废纸篓再放回来（不留痕迹）：`cargo test --lib 真实废纸篓 -- --ignored`
    #[test]
    #[ignore]
    fn 真实废纸篓_移进去再放回原处() {
        let t = TempTree::new();
        let f = t.file(&format!("pika-trash-test-{}", std::process::id()), 3);
        let to = trash_to(&f).unwrap();
        assert!(!f.exists() && to.exists(), "{}", to.display());
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        put_back(&Trashed { from: f.clone(), to: to.clone(), size_bytes: 3 }, &home.join(".Trash")).unwrap();
        assert!(f.exists() && !to.exists());
    }
}
