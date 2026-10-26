//! 优化速度：开机时自动启动的项目。列出、启用/停用、移除、添加。
//!
//! - 启动代理（`~/Library/LaunchAgents`、`/Library/LaunchAgents`）：用 `launchctl disable/enable gui/<uid>/<label>` 开关。
//!   开关记在系统的覆盖表里，不改配置文件、不要密码，随时能改回来。只有用户自己目录里的能移除（移到废纸篓）；
//!   全局目录归 root，只给开关。
//! - 登录时打开的应用（「系统事件」里的登录项）：系统没有「停用」状态。停用 = 从列表里删掉并记下路径，启用时再加回去。
//! - 新式的后台登录项（应用自己用 SMAppService 注册的）只有系统设置能管，界面上留一个打开系统设置的入口。
//! - 系统级后台服务（`/Library/LaunchDaemons`）只列出来给用户看，开关和移除要管理员权限，这里不做。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LoginItem {
    pub name: String,
    /// login_item：登录时打开的应用；agent：在后台常驻的启动代理；daemon：系统级后台服务，只读
    pub kind: &'static str,
    pub enabled: bool,
    /// 全局启动代理（`/Library/LaunchAgents`）：能开关，不能移除。系统级后台服务也是 true
    pub global: bool,
    /// 启动代理的配置文件，或登录时打开的应用本身
    pub path: PathBuf,
    /// 启动代理、系统服务要运行的程序；判断不了时为空
    pub program: Option<PathBuf>,
    /// 程序（登录应用就是应用本身）已经不存在，每次登录都会启动失败
    pub broken: bool,
    /// 启动代理、系统服务属于哪个应用（只用来显示名字，见 [`owner_app`]）；认不出时为空
    pub app: Option<String>,
    /// 登录应用启动后是否隐藏窗口；停用时记下，启用时照原样加回去
    #[serde(skip)]
    pub hidden: bool,
}

/// 优化页上的完整名单。
#[derive(Serialize, Clone, Debug)]
pub struct LoginItems {
    pub items: Vec<LoginItem>,
    /// 读不到「登录时打开的应用」（用户没允许本应用控制「系统事件」，或它没有回应）：这一组没有列出
    pub apps_unreadable: bool,
}

/// 停用掉的「登录时打开的应用」，记下来以便再启用。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedApp {
    pub name: String,
    pub path: PathBuf,
    pub hidden: bool,
}

const SAVED: &str = "disabled-login-items.json";

fn load_saved(dir: &Path) -> Vec<SavedApp> {
    std::fs::read_to_string(dir.join(SAVED)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_saved(dir: &Path, list: &[SavedApp]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join(SAVED), serde_json::to_string_pretty(list).unwrap()).map_err(|e| e.to_string())
}

/// `launchctl print-disabled` 的输出 → 标签 → 是否启用。
/// 新系统写 `=> enabled/disabled`，老系统写 `=> false/true`（true 表示停用）。
fn parse_overrides(output: &str) -> HashMap<String, bool> {
    output
        .lines()
        .filter_map(|l| {
            let (label, state) = l.trim().split_once(" => ")?;
            let on = match state.trim() {
                "enabled" | "false" => true,
                "disabled" | "true" => false,
                _ => return None,
            };
            Some((label.trim_matches('"').to_owned(), on))
        })
        .collect()
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

/// `domain`：用户的启动代理是 [`domain`]，系统级后台服务是 `system`（读不用管理员权限）
fn overrides(domain: &str) -> HashMap<String, bool> {
    Command::new("/bin/launchctl")
        .args(["print-disabled", domain])
        .output()
        .map(|o| parse_overrides(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// 要运行的程序：`Program`，没有就取 `ProgramArguments` 的第一个。相对路径按 `WorkingDirectory` 解析；
/// 只写命令名（不带 /）的由系统按搜索路径找，其余判断不了的情况（如 `BundleProgram`）都返回空，免得把正常的项标成失效
fn program_of(dict: &plist::Dictionary) -> Option<PathBuf> {
    let prog = dict.get("Program").and_then(|v| v.as_string()).or_else(|| dict.get("ProgramArguments")?.as_array()?.first()?.as_string())?;
    let p = Path::new(prog);
    if p.is_absolute() {
        return Some(p.to_owned());
    }
    if !prog.contains('/') {
        return None;
    }
    Some(Path::new(dict.get("WorkingDirectory")?.as_string()?).join(p)).filter(|p| p.is_absolute())
}

/// 确定不存在才算失效；没权限看、读出错都不算
fn missing(path: &Path) -> bool {
    matches!(path.try_exists(), Ok(false))
}

/// 一个启动代理（或系统服务）目录里的每个配置文件算一项。是否启用：覆盖表里有就听它的，否则看配置文件里的 `Disabled`。
/// 本应用自己的开机自启不列出，它在设置里开关。
pub fn launch_agents(dir: &Path, kind: &'static str, global: bool, overrides: &HashMap<String, bool>) -> Vec<LoginItem> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<LoginItem> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "plist"))
        .filter_map(|p| {
            let dict = plist::Value::from_file(&p).ok()?.into_dictionary()?;
            let off = dict.get("Disabled").and_then(|v| v.as_boolean()) == Some(true);
            let name = dict.get("Label").and_then(|v| v.as_string()).map(str::to_owned).unwrap_or_else(|| p.file_stem().unwrap_or_default().to_string_lossy().into_owned());
            if name == crate::autostart::LABEL {
                return None;
            }
            let enabled = overrides.get(&name).copied().unwrap_or(!off);
            let program = program_of(&dict);
            let broken = program.as_deref().is_some_and(missing);
            Some(LoginItem { name, kind, enabled, global, path: p, program, broken, app: None, hidden: false })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 每行「名称 TAB 路径 TAB 是否隐藏」。
fn parse_login_apps(output: &str) -> Vec<SavedApp> {
    output
        .lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let (name, path, hidden) = (f.next()?, f.next()?, f.next()?);
            (!name.is_empty() && path.starts_with('/')).then(|| SavedApp { name: name.to_owned(), path: path.into(), hidden: hidden == "true" })
        })
        .collect()
}

const LIST_APPS: &str = r#"set out to ""
tell application "System Events"
    repeat with i in every login item
        set out to out & (name of i) & tab & (path of i) & tab & (hidden of i) & linefeed
    end repeat
end tell
return out"#;

/// 登录时打开的应用。要向「系统事件」查询，第一次会由系统询问用户是否允许；
/// 用户不允许或迟迟不答时报错，不让界面一直等。超时后不结束查询：系统可能正弹着授权框，结束了框就白弹了
fn login_apps() -> Result<Vec<SavedApp>, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let out = Command::new("/usr/bin/osascript").args(["-e", LIST_APPS]).output();
        let _ = tx.send(out.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()));
    });
    rx.recv_timeout(Duration::from_secs(6)).ok().flatten().map(|s| parse_login_apps(&s)).ok_or_else(|| crate::i18n::t("读不到登录时打开的应用：需要允许本应用控制「系统事件」").into())
}

/// 正在生效的登录应用 + 停用后记下的（已被别处加回去的不重复列）。
fn merge_apps(live: Vec<SavedApp>, saved: Vec<SavedApp>) -> Vec<LoginItem> {
    let item = |a: SavedApp, enabled| LoginItem { broken: missing(&a.path), name: a.name, kind: "login_item", enabled, global: false, path: a.path, program: None, app: None, hidden: a.hidden };
    let off: Vec<SavedApp> = saved.into_iter().filter(|s| !live.iter().any(|l| l.path == s.path)).collect();
    live.into_iter().map(|a| item(a, true)).chain(off.into_iter().map(|a| item(a, false))).collect()
}

/// 已装应用的（包标识（小写），应用名）。应用名和卸载页一样取应用包的文件名。
fn installed_ids(home: &Path) -> Vec<(String, String)> {
    [PathBuf::from("/Applications"), home.join("Applications")]
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|e| e.flatten().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "app"))
        .filter_map(|p| {
            let dict = plist::Value::from_file(p.join("Contents/Info.plist")).ok()?.into_dictionary()?;
            let id = dict.get("CFBundleIdentifier")?.as_string()?.to_lowercase();
            Some((id, p.file_stem()?.to_string_lossy().into_owned()))
        })
        .collect()
}

/// 启动代理属于哪个应用，只用来显示名字（开关、移除照旧只认配置文件）：
/// 1. 要运行的程序在某个应用包里，就是最外层那个应用包；
/// 2. 否则标签就是某个已装应用的包标识，或以它开头、后面跟着 `.` `-` `_`（取最长的那个）。
/// 不按「同一开发者」猜：实测 com.sangfor.aTrustUninstallMonitor 会被认成同开发者的 EasyConnect，认错比不认更糟。
fn owner_app(label: &str, program: Option<&Path>, apps: &[(String, String)]) -> Option<String> {
    if let Some(app) = program.and_then(|p| p.ancestors().filter(|a| a.extension().is_some_and(|x| x == "app")).last()) {
        return Some(app.file_stem()?.to_string_lossy().into_owned());
    }
    let label = label.to_lowercase();
    let starts = |id: &str| label == id || label.strip_prefix(id).is_some_and(|rest| rest.starts_with(['.', '-', '_']));
    apps.iter().filter(|(id, _)| starts(id)).max_by_key(|(id, _)| id.len()).map(|(_, name)| name.clone())
}

/// 某一种启动项。开关、移除前按种类重新列：启动代理和系统服务只读配置文件，不去问「系统事件」。
/// `data_dir` 是本应用的数据目录，停用掉的登录应用记在那里。
pub fn list(kind: &str, home: &Path, data_dir: &Path) -> Result<Vec<LoginItem>, String> {
    Ok(match kind {
        "login_item" => merge_apps(login_apps()?, load_saved(data_dir)),
        "agent" => {
            let ov = overrides(&domain());
            let mut items = launch_agents(&home.join("Library/LaunchAgents"), "agent", false, &ov);
            items.extend(launch_agents(Path::new("/Library/LaunchAgents"), "agent", true, &ov));
            with_apps(items, home)
        }
        "daemon" => with_apps(launch_agents(Path::new("/Library/LaunchDaemons"), "daemon", true, &overrides("system")), home),
        _ => Vec::new(),
    })
}

fn with_apps(mut items: Vec<LoginItem>, home: &Path) -> Vec<LoginItem> {
    let apps = installed_ids(home);
    for i in &mut items {
        i.app = owner_app(&i.name, i.program.as_deref(), &apps);
    }
    items
}

pub fn login_items(home: &Path, data_dir: &Path) -> LoginItems {
    let apps = list("login_item", home, data_dir);
    let apps_unreadable = apps.is_err();
    let mut items = apps.unwrap_or_default();
    for kind in ["agent", "daemon"] {
        items.extend(list(kind, home, data_dir).unwrap_or_default());
    }
    LoginItems { items, apps_unreadable }
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let o = cmd.output().map_err(|e| e.to_string())?;
    if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()) }
}

/// 让「系统事件」执行一句话。参数从 argv 传进去，不拼进脚本，路径里有引号也不会出事。
fn system_events(statement: &str, args: &[&str]) -> Result<(), String> {
    run(Command::new("/usr/bin/osascript")
        .args(["-e", "on run argv", "-e", &format!("tell application \"System Events\" to {statement}"), "-e", "end run"])
        .args(args))
}

fn add_app(path: &Path, hidden: bool) -> Result<(), String> {
    system_events("make login item at end with properties {path:(item 1 of argv), hidden:((item 2 of argv) is \"true\")}", &[&path.to_string_lossy(), if hidden { "true" } else { "false" }])
}

fn delete_app(path: &Path) -> Result<(), String> {
    system_events("delete (every login item whose path is (item 1 of argv))", &[&path.to_string_lossy()])
}

/// 系统级后台服务归 root，开关和移除都要管理员权限，这里只给看
fn read_only(item: &LoginItem) -> Result<(), String> {
    if item.kind == "daemon" {
        return Err(crate::i18n::t("系统级后台服务只能查看，开关和移除需要管理员权限").into());
    }
    Ok(())
}

/// 启用或停用。`item` 必须来自 [`list`] 刚列出的名单（调用方负责查）。
pub fn set_enabled(item: &LoginItem, on: bool, data_dir: &Path) -> Result<(), String> {
    read_only(item)?;
    if item.enabled == on {
        return Ok(());
    }
    if item.kind == "agent" {
        let target = format!("{}/{}", domain(), item.name);
        run(Command::new("/bin/launchctl").args([if on { "enable" } else { "disable" }, &target]))?;
        // 立即生效：停用时停掉正在跑的，启用时马上加载。没在跑、已加载这类失败不影响下次登录，忽略
        let _ = if on { run(Command::new("/bin/launchctl").arg("bootstrap").arg(domain()).arg(&item.path)) } else { run(Command::new("/bin/launchctl").args(["bootout", &target])) };
        return Ok(());
    }
    let mut saved = load_saved(data_dir);
    if on {
        let hidden = saved.iter().find(|s| s.path == item.path).is_some_and(|s| s.hidden);
        add_app(&item.path, hidden)?;
        saved.retain(|s| s.path != item.path);
    } else {
        // 先记下再删，删失败也只是多一条记录，不会丢
        saved.push(SavedApp { name: item.name.clone(), path: item.path.clone(), hidden: item.hidden });
        save_saved(data_dir, &saved)?;
        return delete_app(&item.path);
    }
    save_saved(data_dir, &saved)
}

/// 移除：启动代理停掉并把配置文件移到废纸篓；登录应用从列表里删掉（应用本身不动）。
pub fn remove(item: &LoginItem, data_dir: &Path) -> Result<(), String> {
    read_only(item)?;
    if item.kind == "agent" {
        if item.global {
            return Err(crate::i18n::t("全局启动代理不能移除，只能停用").into());
        }
        let _ = run(Command::new("/bin/launchctl").args(["bootout", &format!("{}/{}", domain(), item.name)]));
        return crate::remove::trash_real(&item.path);
    }
    if item.enabled {
        delete_app(&item.path)?;
    }
    let mut saved = load_saved(data_dir);
    saved.retain(|s| s.path != item.path);
    save_saved(data_dir, &saved)
}

/// 弹出系统的「选择应用」窗口，把选中的应用加进登录时打开的列表。用户取消时返回 false。
pub fn add_chosen() -> Result<bool, String> {
    let o = Command::new("/usr/bin/osascript")
        .args(["-e", "tell application \"System Events\"", "-e", "activate", "-e", "POSIX path of (choose application as alias)", "-e", "end tell"])
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        // -128：用户点了取消
        return if err.contains("-128") { Ok(false) } else { Err(err.trim().to_owned()) };
    }
    let path = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim().trim_end_matches('/'));
    if path.extension().is_none_or(|x| x != "app") || !path.is_dir() {
        return Err(crate::i18n::t("选中的不是应用").into());
    }
    if !login_apps()?.iter().any(|a| a.path == path) {
        add_app(&path, false)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 认出启动代理属于哪个应用() {
        let apps: Vec<(String, String)> = [("io.github.clash-verge-rev.clash-verge-rev", "Clash Verge"), ("com.docker.docker", "Docker"), ("top.daoly.starcode", "starCode"), ("com.google.chrome", "Google Chrome"), ("com.google.drivefs", "Google Drive")]
            .map(|(a, b)| (a.into(), b.into()))
            .into();
        let own = |label: &str, prog: Option<&str>| owner_app(label, prog.map(Path::new), &apps);
        // 程序在应用包里：取最外层的应用包
        assert_eq!(own("com.sogou.SogouTaskManager", Some("/Library/Input Methods/SogouInput.app/Contents/SogouTaskManager.app/Contents/MacOS/x")).as_deref(), Some("SogouInput"));
        // 标签以包标识开头
        assert_eq!(own("io.github.clash-verge-rev.clash-verge-rev.service", Some("/Library/PrivilegedHelperTools/x")).as_deref(), Some("Clash Verge"));
        assert_eq!(own("top.daoly.starcode-daemon", None).as_deref(), Some("starCode"));
        // 只是同一开发者（包标识前两段相同）不算，不猜
        assert_eq!(own("com.docker.vmnetd", None), None);
        assert_eq!(own("com.google.GoogleUpdater.wake", None), None);
        assert_eq!(own("com.jcode.hotkey", Some("/Users/me/.jcode/jcode")), None);
        assert_eq!(own("com.apple.something", None), None);
    }
    use crate::scan::testutil::TempTree;

    #[test]
    fn 读取启动代理和它的开关状态() {
        let t = TempTree::new();
        let plist = |label: &str, extra: &str| format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>Label</key><string>{label}</string>{extra}</dict></plist>"#);
        std::fs::write(t.0.join("b.plist"), plist("com.example.sync", "")).unwrap();
        std::fs::write(t.0.join("a.plist"), plist("com.example.off", "<key>Disabled</key><true/>")).unwrap();
        std::fs::write(t.0.join("c.plist"), plist("com.example.overridden", "")).unwrap();
        std::fs::write(t.0.join("self.plist"), plist(crate::autostart::LABEL, "")).unwrap();
        std::fs::write(t.0.join("nolabel.plist"), r#"<?xml version="1.0"?><plist version="1.0"><dict></dict></plist>"#).unwrap();
        std::fs::write(t.0.join("broken.plist"), "不是配置文件").unwrap();
        std::fs::write(t.0.join("readme.txt"), "x").unwrap();
        let ov = parse_overrides("disabled services = {\n\t\t\"com.example.overridden\" => disabled\n\t\t\"com.example.off\" => false\n\t}\n");
        let got: Vec<(String, bool)> = launch_agents(&t.0, "agent", false, &ov).into_iter().map(|i| (i.name, i.enabled)).collect();
        let want = [("com.example.off", true), ("com.example.overridden", false), ("com.example.sync", true), ("nolabel", true)];
        assert_eq!(got, want.map(|(n, e)| (n.to_owned(), e)));
        assert!(launch_agents(&t.0.join("不存在"), "agent", false, &ov).is_empty());
    }

    #[test]
    fn 读出要运行的程序并标出已不存在的() {
        let t = TempTree::new();
        let bin = t.file("app/bin/run", 1);
        let dir = t.0.join("agents");
        std::fs::create_dir_all(&dir).unwrap();
        let plist = |label: &str, extra: &str| format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>Label</key><string>{label}</string>{extra}</dict></plist>"#);
        let args = |a: &str| format!("<key>ProgramArguments</key><array><string>{a}</string><string>--flag</string></array>");
        let cases = [
            ("a.ok", format!("<key>Program</key><string>{}</string>", bin.display())),
            ("b.gone", args(&t.0.join("app/bin/没了").display().to_string())),
            ("c.relative", format!("{}<key>WorkingDirectory</key><string>{}</string>", args("./bin/run"), t.0.join("app").display())),
            ("d.relative_gone", format!("{}<key>WorkingDirectory</key><string>{}</string>", args("./bin/没了"), t.0.join("app").display())),
            // 只写命令名、没有工作目录的相对路径、只有 BundleProgram：判断不了，不算失效
            ("e.bare", args("sh")),
            ("f.no_dir", args("./bin/run")),
            ("g.bundle", "<key>BundleProgram</key><string>Contents/MacOS/x</string>".to_owned()),
        ];
        for (label, extra) in &cases {
            std::fs::write(dir.join(format!("{label}.plist")), plist(label, extra)).unwrap();
        }
        let items = launch_agents(&dir, "daemon", true, &HashMap::new());
        let got: Vec<(String, bool, bool)> = items.iter().map(|i| (i.name.clone(), i.program.is_some(), i.broken)).collect();
        let want = [("a.ok", true, false), ("b.gone", true, true), ("c.relative", true, false), ("d.relative_gone", true, true), ("e.bare", false, false), ("f.no_dir", false, false), ("g.bundle", false, false)];
        assert_eq!(got, want.map(|(n, p, b)| (n.to_owned(), p, b)));
        assert_eq!(items[0].kind, "daemon");
        assert!(set_enabled(&items[0], false, &t.0).is_err() && remove(&items[0], &t.0).is_err(), "系统级后台服务只读");
    }

    #[test]
    fn 解析登录应用并合并停用掉的() {
        let live = parse_login_apps("iPhone镜像\t/System/Applications/iPhone Mirroring.app\tfalse\n闪电右键\t/Applications/QuickRight.app\ttrue\n坏行\n");
        assert_eq!(live.len(), 2);
        assert!(live[1].hidden);
        let saved = vec![
            SavedApp { name: "Docker".into(), path: "/Applications/Docker.app".into(), hidden: false },
            // 用户在系统设置里又加回去了：不重复列
            SavedApp { name: "闪电右键".into(), path: "/Applications/QuickRight.app".into(), hidden: true },
        ];
        let merged = merge_apps(live, saved);
        assert!(merged[1].hidden, "停用时照原样记下是否隐藏");
        let gone = SavedApp { name: "没了".into(), path: "/不存在/没了.app".into(), hidden: false };
        assert!(merge_apps(vec![], vec![gone])[0].broken, "应用已经不在了");
        let got: Vec<(String, bool)> = merged.into_iter().map(|i| (i.name, i.enabled)).collect();
        assert_eq!(got, [("iPhone镜像".to_owned(), true), ("闪电右键".to_owned(), true), ("Docker".to_owned(), false)]);
    }

    #[test]
    #[ignore]
    fn 在本机真实读一遍开机启动项() {
        let t = TempTree::new();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        let all = login_items(&home, &t.0);
        println!("读不到登录应用：{}", all.apps_unreadable);
        for i in all.items {
            println!("{:<10} {:<5} {:<5} {:<5} {} {} {:?} {:?}", i.kind, i.enabled, i.global, i.broken, i.name, i.path.display(), i.program, i.app);
        }
    }

    #[test]
    fn 停用记录能存能读() {
        let t = TempTree::new();
        assert!(load_saved(&t.0).is_empty());
        let list = vec![SavedApp { name: "A".into(), path: "/Applications/A.app".into(), hidden: true }];
        save_saved(&t.0, &list).unwrap();
        assert_eq!(load_saved(&t.0), list);
    }
}
