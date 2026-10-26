//! 需要管理员权限的操作的唯一入口（规格不变量 6、决定 D-04）。
//!
//! 没有正式签名就装不了常驻的特权助手，所以默认每次都由系统弹出密码框。
//! 用户可以授权一次，让释放内存和刷新 DNS 以后不再问密码（D-04 的修订，见下面的「免密」）。
//! 能执行的命令是下面写死的几条，不接受任何外部传入的命令或参数。

use crate::i18n::t;
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Deserialize, Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    PurgeMemory,
    FlushDns,
    RebuildSpotlight,
    ThinSnapshots,
}

pub const ALL: [Task; 4] = [Task::PurgeMemory, Task::FlushDns, Task::RebuildSpotlight, Task::ThinSnapshots];

impl Task {
    /// 以管理员身份执行的命令。全部是常量。
    fn script(self) -> &'static str {
        match self {
            Task::PurgeMemory => "/usr/sbin/purge",
            Task::FlushDns => "/usr/bin/dscacheutil -flushcache; /usr/bin/killall -HUP mDNSResponder",
            Task::RebuildSpotlight => "/usr/bin/mdutil -E /",
            Task::ThinSnapshots => "/usr/bin/tmutil thinlocalsnapshots / 999999999999 4",
        }
    }

    /// 用户授权免密后，不经密码框、直接用 sudo 执行的命令；空表示这项任务始终要输密码。
    /// 只放被别的程序拿去乱用也没有损害的两项：重建索引会让机器忙上几小时，回收快照删了就回不来，都不放。
    fn sudo_commands(self) -> &'static [&'static [&'static str]] {
        match self {
            Task::PurgeMemory => &[&["/usr/sbin/purge"]],
            Task::FlushDns => &[&["/usr/bin/dscacheutil", "-flushcache"], &["/usr/bin/killall", "-HUP", "mDNSResponder"]],
            Task::RebuildSpotlight | Task::ThinSnapshots => &[],
        }
    }

    /// 显示在系统密码框里的说明：要做什么。
    pub fn reason(self) -> &'static str {
        match self {
            Task::PurgeMemory => t("Pika Clean 要释放未使用的内存。"),
            Task::FlushDns => t("Pika Clean 要刷新 DNS 缓存。"),
            Task::RebuildSpotlight => t("Pika Clean 要重建聚焦搜索的索引。"),
            Task::ThinSnapshots => t("Pika Clean 要回收本地快照占用的可清除空间。"),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum AdminError {
    /// 用户在密码框里点了取消
    Cancelled,
    Failed(String),
}

fn source(task: Task) -> String {
    format!("do shell script \"{}\" with prompt \"{}\" with administrator privileges", task.script(), task.reason())
}

/// 系统缓存所在的目录。以管理员身份删除时只认它的直接子项。
const SYSTEM_CACHE_DIR: &str = "/Library/Caches";
/// 崩溃转储所在的目录。以管理员身份删除时只认它下面直接的 `core.<数字>` 文件。
pub const CORES_DIR: &str = "/cores";
/// 一次最多删这么多项，免得命令太长。
const MAX_PATHS: usize = 200;

/// 系统写崩溃转储用的名字：`core.` 加进程号。
pub fn is_core_dump_name(name: &str) -> bool {
    name.strip_prefix("core.").is_some_and(|pid| (1..=10).contains(&pid.len()) && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// 把要删的路径变成一条命令，连同密码框上的说明。任何一个路径不合规就整批拒绝，一个都不删。
///
/// 这是全应用最危险的一处：命令会以 root 身份执行。所以规则写得很死——
/// 路径必须是「/Library/Caches/名字」，名字里只能有字母、数字、点、下划线、连字符和空格；
/// 或者是「/cores/core.数字」。这样拼进命令时不可能带上引号、斜杠、通配符或 `..`。
fn remove_script(paths: &[std::path::PathBuf]) -> Result<(String, &'static str), AdminError> {
    if paths.is_empty() || paths.len() > MAX_PATHS {
        return Err(AdminError::Failed(t("要删除的条目数量不对").into()));
    }
    let (mut caches, mut cores) = (String::new(), String::new());
    for p in paths {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let parent = p.parent().and_then(|d| d.to_str());
        let ok_cache = !name.is_empty() && name != "." && name != ".." && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' '));
        match parent {
            Some(SYSTEM_CACHE_DIR) if ok_cache => caches.push_str(&format!(" '{SYSTEM_CACHE_DIR}/{name}'")),
            Some(CORES_DIR) if is_core_dump_name(name) => cores.push_str(&format!(" '{CORES_DIR}/{name}'")),
            _ => return Err(AdminError::Failed(format!("{}{}", t("拒绝以管理员身份删除这个路径："), p.display()))),
        }
    }
    // 崩溃转储是普通文件，不带 -r：万一在密码框弹出期间被换成了目录，也不会删进去；rm 本身不跟随符号链接
    Ok(match (caches.is_empty(), cores.is_empty()) {
        (false, true) => (format!("/bin/rm -rf --{caches}"), t("Pika Clean 要清理所有用户共用的系统缓存。")),
        (true, false) => (format!("/bin/rm -f --{cores}"), t("Pika Clean 要删除程序崩溃时留下的内存转储。")),
        _ => (format!("/bin/rm -rf --{caches}; /bin/rm -f --{cores}"), t("Pika Clean 要清理系统缓存和程序崩溃时留下的内存转储。")),
    })
}

/// 扫描结果里要以管理员身份删的类别，去掉 [`remove_script`] 一定会拒绝的条目（名字里带引号、括号、中文等），
/// 免得混进一个就整批被拒、一项都删不掉。判断只在这里写一份；删除前 `remove_script` 仍会再查一遍。
pub fn retain_removable(report: &mut crate::model::ScanReport) {
    for c in report.categories.iter_mut().filter(|c| c.needs_admin) {
        c.items.retain(|i| remove_script(std::slice::from_ref(&i.path)).is_ok());
        c.total_bytes = c.items.iter().map(|i| i.size_bytes).sum();
    }
}

fn remove_source((script, prompt): &(String, &str)) -> String {
    format!("do shell script \"{script}\" with prompt \"{prompt}\" with administrator privileges")
}

/// 以管理员身份删除系统缓存目录下的条目和崩溃转储。系统只弹一次密码框。
pub fn remove_as_admin(paths: &[std::path::PathBuf]) -> Result<(), AdminError> {
    let source = remove_source(&remove_script(paths)?);
    let out = Command::new("/usr/bin/osascript").args(["-e", &source]).output().map_err(|e| AdminError::Failed(e.to_string()))?;
    interpret(out.status.success(), &String::from_utf8_lossy(&out.stderr))
}

fn interpret(success: bool, stderr: &str) -> Result<(), AdminError> {
    if success {
        Ok(())
    } else if stderr.contains("-128") || stderr.contains("User canceled") {
        Err(AdminError::Cancelled)
    } else {
        Err(AdminError::Failed(stderr.trim().chars().take(200).collect()))
    }
}

/// 免密规则文件。它存在就表示用户授权过。
const SUDOERS_FILE: &str = "/etc/sudoers.d/pikaclean";

/// 免密规则：只允许这一个用户、以 root 身份、免密执行 `sudo_commands` 里的命令，参数必须一字不差。
/// 用户用编号而不是名字，免得把用户名拼进以 root 身份执行的脚本。
fn sudoers_rule(uid: u32) -> String {
    let commands: Vec<String> = ALL
        .iter()
        .flat_map(|task| task.sudo_commands())
        // 规则里只写程序路径表示「带什么参数都行」，后面跟一对空引号才是「不许带参数」
        .map(|argv| if argv.len() == 1 { format!("{} \"\"", argv[0]) } else { argv.join(" ") })
        .collect();
    format!("#{uid} ALL=(root) NOPASSWD: {}", commands.join(", "))
}

/// 写入或删除免密规则的命令。写入时先写到同目录下带点的临时名字（系统不读名字里带点的规则文件），
/// 校验通过才改成正式名字：规则写坏了会让整台机器的 sudo 都用不了。
fn passwordless_script(on: bool, uid: u32) -> String {
    if !on {
        return format!("/bin/rm -f {SUDOERS_FILE}");
    }
    let tmp = format!("{SUDOERS_FILE}.tmp");
    format!(
        "/bin/echo '{}' > {tmp} && /bin/chmod 0440 {tmp} && /usr/sbin/visudo -cf {tmp} && /bin/mv -f {tmp} {SUDOERS_FILE} || {{ /bin/rm -f {tmp}; exit 1; }}",
        sudoers_rule(uid)
    )
}

fn passwordless_source(on: bool, uid: u32) -> String {
    let prompt = if on { t("Pika Clean 要设置以后释放内存和刷新 DNS 缓存时不再询问密码。") } else { t("Pika Clean 要撤销免密授权。") };
    // 规则里有一对引号，放进 AppleScript 的字符串要转义
    format!("do shell script \"{}\" with prompt \"{prompt}\" with administrator privileges", passwordless_script(on, uid).replace('"', "\\\""))
}

/// 用户是否授权过免密。
pub fn passwordless() -> bool {
    std::path::Path::new(SUDOERS_FILE).exists()
}

/// 授权免密后不再问密码的任务。
pub fn passwordless_tasks() -> Vec<Task> {
    ALL.into_iter().filter(|task| !task.sudo_commands().is_empty()).collect()
}

/// 开启或撤销免密。两种情况系统都会弹一次密码框。
pub fn set_passwordless(on: bool) -> Result<(), AdminError> {
    // SAFETY: getuid 没有参数，也不会失败。
    let source = passwordless_source(on, unsafe { libc::getuid() });
    let out = Command::new("/usr/bin/osascript").args(["-e", &source]).output().map_err(|e| AdminError::Failed(e.to_string()))?;
    interpret(out.status.success(), &String::from_utf8_lossy(&out.stderr))
}

/// 执行一项任务。授权过免密的任务直接执行，其余的弹出系统密码框，阻塞到用户输入密码或取消为止。
pub fn run(task: Task) -> Result<(), AdminError> {
    let free = task.sudo_commands();
    // `-n`：要密码时立刻失败而不是等输入。规则失效了（比如被手动删改）就退回到下面的密码框
    let sudo = |argv: &&[&str]| Command::new("/usr/bin/sudo").arg("-n").args(*argv).output().is_ok_and(|o| o.status.success());
    if !free.is_empty() && passwordless() && free.iter().all(sudo) {
        return Ok(());
    }
    let out = Command::new("/usr/bin/osascript").args(["-e", &source(task)]).output().map_err(|e| AdminError::Failed(e.to_string()))?;
    interpret(out.status.success(), &String::from_utf8_lossy(&out.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 命令和说明都是不含引号与反斜杠的常量() {
        for task in ALL {
            for text in [task.script(), task.reason()] {
                assert!(!text.contains(['"', '\\', '`', '$']), "{task:?} 的文字会破坏脚本的引号：{text}");
            }
            assert!(task.script().starts_with('/'), "命令用绝对路径，不依赖 PATH");
            assert!(source(task).ends_with("with administrator privileges"));
        }
    }

    #[test]
    fn 系统缓存的删除命令只接受规定目录下的简单名字() {
        let p = |s: &str| std::path::PathBuf::from(s);
        let script = remove_script(&[p("/Library/Caches/com.apple.iconservices.store"), p("/Library/Caches/Desktop Pictures")]).unwrap().0;
        assert_eq!(script, "/bin/rm -rf -- '/Library/Caches/com.apple.iconservices.store' '/Library/Caches/Desktop Pictures'");
        assert!(!script.contains(['"', '\\', '`', '$', '*']));
        for bad in [
            "/Library/Caches",
            "/Library/Caches/..",
            "/Library/Caches/a/b",
            "/Library/Caches/../../etc",
            "/etc/hosts",
            "/Library/Caches/it's",
            "/Library/Caches/a\"b",
            "/Library/Caches/$(reboot)",
            "/Library/Caches/*",
            "Library/Caches/x",
        ] {
            assert!(remove_script(&[p(bad)]).is_err(), "应当拒绝 {bad}");
            // 一批里混进一个不合规的，整批拒绝
            assert!(remove_script(&[p("/Library/Caches/ok"), p(bad)]).is_err());
        }
        assert!(remove_script(&[]).is_err());
        assert!(remove_script(&vec![p("/Library/Caches/x"); MAX_PATHS + 1]).is_err());
    }

    #[test]
    fn 扫描结果去掉管理员脚本会拒绝的系统缓存() {
        use crate::model::{Category, CategoryId, Item, ScanReport};
        let item = |p: &str| Item { id: p.into(), path: p.into(), size_bytes: 10, last_used_ms: None, partial: false };
        let cat = |id, root: &str, needs_admin, items| Category { id, root: root.into(), total_bytes: 30, items, default_selected: false, removal: crate::remove::mode_of(id), needs_admin, needs_full_disk_access: false, unreadable: false, partial: false, only: vec![] };
        let mut r = ScanReport {
            categories: vec![
                cat(CategoryId::SystemCache, "/Library/Caches", true, vec![item("/Library/Caches/ok"), item("/Library/Caches/a(1)"), item("/Library/Caches/中文")]),
                cat(CategoryId::Logs, "/x", false, vec![item("/x/a(1)")]),
            ],
            ..Default::default()
        };
        retain_removable(&mut r);
        assert_eq!(r.categories[0].items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["/Library/Caches/ok"]);
        assert_eq!(r.categories[0].total_bytes, 10);
        assert_eq!(r.categories[1].items.len(), 1, "不用管理员删的类别不动");
    }

    #[test]
    fn 崩溃转储的删除命令只接受_cores_下的_core_数字() {
        let p = |s: &str| std::path::PathBuf::from(s);
        let (script, prompt) = remove_script(&[p("/cores/core.4821"), p("/cores/core.1")]).unwrap();
        assert_eq!(script, "/bin/rm -f -- '/cores/core.4821' '/cores/core.1'", "不带 -r");
        assert_eq!(prompt, "Pika Clean 要删除程序崩溃时留下的内存转储。");
        let (mixed, _) = remove_script(&[p("/cores/core.7"), p("/Library/Caches/x")]).unwrap();
        assert_eq!(mixed, "/bin/rm -rf -- '/Library/Caches/x'; /bin/rm -f -- '/cores/core.7'");
        for bad in [
            "/cores",
            "/cores/core.",
            "/cores/core",
            "/cores/core.12a",
            "/cores/core.-1",
            "/cores/core.12345678901",
            "/cores/..",
            "/cores/../etc/core.1",
            "/cores/sub/core.1",
            "/cores/x",
            "/tmp/core.1",
            "/Library/Caches/core.1/x",
            "cores/core.1",
            "/private/cores/core.1",
        ] {
            assert!(remove_script(&[p(bad)]).is_err(), "应当拒绝 {bad}");
            assert!(remove_script(&[p("/cores/core.1"), p(bad)]).is_err());
        }
    }

    #[test]
    fn 崩溃转储的删除命令不删目录也不跟随符号链接() {
        // 把 /cores 换成临时目录，不提权把同一条命令真跑一遍
        let dir = std::env::temp_dir().join(format!("pika-cores-{}", std::process::id()));
        let outside = dir.with_extension("outside");
        std::fs::create_dir_all(dir.join("core.2")).unwrap();
        std::fs::write(dir.join("core.1"), b"x").unwrap();
        std::fs::write(&outside, b"keep").unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("core.3")).unwrap();
        let p = |s: &str| std::path::PathBuf::from(s);
        let script = remove_script(&[p("/cores/core.1"), p("/cores/core.2"), p("/cores/core.3")]).unwrap().0.replace("/cores/", &format!("{}/", dir.display()));
        let _ = Command::new("/bin/sh").args(["-c", &script]).status().unwrap();
        let state = (dir.join("core.1").exists(), dir.join("core.2").is_dir(), dir.join("core.3").symlink_metadata().is_ok(), outside.exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&outside);
        assert_eq!(state, (false, true, false, true), "文件删掉、目录留着、链接只删链接本身");
    }

    #[test]
    fn 免密规则只放行释放内存和刷新_dns() {
        assert_eq!(passwordless_tasks(), [Task::PurgeMemory, Task::FlushDns]);
        assert_eq!(sudoers_rule(501), r#"#501 ALL=(root) NOPASSWD: /usr/sbin/purge "", /usr/bin/dscacheutil -flushcache, /usr/bin/killall -HUP mDNSResponder"#);
        // 免密执行的和弹密码框执行的是同一批命令
        for task in passwordless_tasks() {
            let joined: Vec<String> = task.sudo_commands().iter().map(|argv| argv.join(" ")).collect();
            assert_eq!(joined.join("; "), task.script());
        }
        // 拼进脚本时用单引号包着，规则里不能有会提前结束引号或被 AppleScript 当成转义的字符
        assert!(!sudoers_rule(501).contains(['\'', '\\', '`', '$']));
        assert_eq!(passwordless_script(false, 501), "/bin/rm -f /etc/sudoers.d/pikaclean");
    }

    #[test]
    fn 免密规则的写入命令能通过系统的语法检查并写对文件() {
        // 把目标换成临时目录，不提权把同一条命令真跑一遍（其中就有系统对规则的语法检查）
        use std::os::unix::fs::PermissionsExt;
        let file = std::env::temp_dir().join(format!("pika-sudoers-{}", std::process::id()));
        let script = passwordless_script(true, 501).replace(SUDOERS_FILE, file.to_str().unwrap());
        let ran = Command::new("/bin/sh").args(["-c", &script]).output().unwrap();
        let state = (std::fs::read_to_string(&file).ok(), std::fs::metadata(&file).map(|m| m.permissions().mode() & 0o777).ok(), file.with_extension("tmp").exists());
        let _ = std::fs::remove_file(&file);
        assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
        assert_eq!(state, (Some(sudoers_rule(501) + "\n"), Some(0o440), false));
    }

    // 下面三项是「不输密码也能验证」的部分：真正以管理员身份执行那一步仍然要人来试。

    #[test]
    fn 命令里用到的程序这台系统上都有() {
        for task in ALL {
            for cmd in task.script().split(';') {
                let program = cmd.split_whitespace().next().unwrap();
                assert!(std::path::Path::new(program).is_file(), "{task:?} 要用的 {program} 不存在");
            }
        }
        assert!(std::path::Path::new("/bin/rm").is_file() && std::path::Path::new("/usr/bin/osascript").is_file());
    }

    #[test]
    fn 交给系统的提权脚本能通过编译() {
        // 只编译不执行：检查整句脚本（含密码框上的说明）语法正确，不会弹密码框
        let out = std::env::temp_dir().join(format!("pika-admin-{}.scpt", std::process::id()));
        let p = std::path::PathBuf::from;
        let removes = [vec![p("/Library/Caches/Desktop Pictures")], vec![p("/cores/core.123")], vec![p("/Library/Caches/x"), p("/cores/core.1")]].map(|ps| remove_source(&remove_script(&ps).unwrap()));
        for text in ALL.map(source).into_iter().chain(removes).chain([passwordless_source(true, 501), passwordless_source(false, 501)]) {
            let ok = Command::new("/usr/bin/osacompile").args(["-e", &text, "-o"]).arg(&out).output().unwrap();
            assert!(ok.status.success(), "编译不过：{text}\n{}", String::from_utf8_lossy(&ok.stderr));
        }
        let _ = std::fs::remove_file(out);
    }

    #[test]
    fn 系统缓存的删除命令只删掉指定的条目() {
        // 这台系统的 /Library/Caches 允许普通用户放自己的东西，所以能不提权就把同一条命令真跑一遍。
        // 只动自己刚建的两个带进程号的目录。不可写的系统上跳过。
        let dir = |name: &str| std::path::PathBuf::from(format!("{SYSTEM_CACHE_DIR}/pika-test-{}-{name}", std::process::id()));
        let (target, spaced, keep) = (dir("target"), dir("with space"), dir("keep"));
        if std::fs::create_dir_all(target.join("sub")).is_err() {
            return eprintln!("跳过：{SYSTEM_CACHE_DIR} 不可写");
        }
        for d in [&spaced, &keep] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(target.join("sub/file"), b"x").unwrap();
        let script = remove_script(&[target.clone(), spaced.clone()]).unwrap().0;
        let ran = Command::new("/bin/sh").args(["-c", &script]).status().unwrap();
        let state = (ran.success(), target.exists(), spaced.exists(), keep.exists());
        let _ = std::fs::remove_dir_all(&keep);
        assert_eq!(state, (true, false, false, true));
    }

    #[test]
    fn 区分用户取消和真正的失败() {
        assert_eq!(interpret(true, ""), Ok(()));
        assert_eq!(interpret(false, "0:95: execution error: User canceled. (-128)"), Err(AdminError::Cancelled));
        assert_eq!(interpret(false, "mdutil: 出错了"), Err(AdminError::Failed("mdutil: 出错了".into())));
    }

    #[test]
    fn 任务名与前端约定的写法一致() {
        assert_eq!(serde_json::to_string(&ALL).unwrap(), r#"["purge_memory","flush_dns","rebuild_spotlight","thin_snapshots"]"#);
        assert_eq!(serde_json::from_str::<Task>(r#""flush_dns""#).unwrap(), Task::FlushDns);
        assert!(serde_json::from_str::<Task>(r#""rm -rf /""#).is_err());
    }
}
