//! 开机自启：往用户的启动代理目录里放（或拿走）一个配置文件，让系统在登录时以 `--hidden` 启动本应用。

use std::path::{Path, PathBuf};

pub const LABEL: &str = "com.pikaclean.mac";

fn file(agents_dir: &Path) -> PathBuf {
    agents_dir.join(format!("{LABEL}.plist"))
}

/// 配置文件里记的程序路径；文件不在或读不懂时为空。
fn recorded_exe(agents_dir: &Path) -> Option<PathBuf> {
    let dict = plist::Value::from_file(file(agents_dir)).ok()?.into_dictionary()?;
    Some(PathBuf::from(dict.get("ProgramArguments")?.as_array()?.first()?.as_string()?))
}

/// 配置文件在、并且里面记的程序还在，才算开着：应用被挪走后登录时其实起不来。
pub fn is_enabled(agents_dir: &Path) -> bool {
    recorded_exe(agents_dir).is_some_and(|p| p.exists())
}

/// 从下载目录直接打开（系统会把应用临时挪到一个随机目录运行）或从磁盘映像里运行时，程序路径过后就不在了，不能写进开机自启。
pub fn is_temporary(exe: &Path) -> bool {
    let s = exe.to_string_lossy();
    s.contains("/AppTranslocation/") || s.starts_with("/Volumes/")
}

/// 开着开机自启、但记的程序路径已经不在了（应用被挪过地方），就改成现在的。
/// 路径还在时不动：可能是本机有好几份应用，不能悄悄改到正在运行的这一份上。
pub fn repair(agents_dir: &Path, exe: &Path) -> std::io::Result<()> {
    match recorded_exe(agents_dir) {
        Some(old) if !old.exists() && !is_temporary(exe) => set(agents_dir, exe, true),
        _ => Ok(()),
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn set(agents_dir: &Path, exe: &Path, on: bool) -> std::io::Result<()> {
    if !on {
        return match std::fs::remove_file(file(agents_dir)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(agents_dir)?;
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>--hidden</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
        xml_escape(&exe.to_string_lossy())
    );
    std::fs::write(file(agents_dir), plist)
}

/// 改名前装的启动代理还在的话，换成现在的。旧文件里记的是旧的程序路径，留着只会让登录时启动失败。
pub fn migrate_from(agents_dir: &Path, old_label: &str, exe: &Path) -> std::io::Result<()> {
    let old = agents_dir.join(format!("{old_label}.plist"));
    if old.is_file() {
        set(agents_dir, exe, true)?;
        std::fs::remove_file(old)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::testutil::TempTree;

    #[test]
    fn 开关开机自启() {
        let t = TempTree::new();
        let dir = t.0.join("LaunchAgents");
        assert!(!is_enabled(&dir));
        set(&dir, Path::new("/Applications/A & B.app/Contents/MacOS/x"), true).unwrap();
        assert!(file(&dir).is_file());
        // 写出来的文件系统能读懂，路径里的特殊字符没有破坏格式
        let dict = plist::Value::from_file(file(&dir)).unwrap().into_dictionary().unwrap();
        let args: Vec<&str> = dict["ProgramArguments"].as_array().unwrap().iter().filter_map(|v| v.as_string()).collect();
        assert_eq!(args, ["/Applications/A & B.app/Contents/MacOS/x", "--hidden"]);
        assert_eq!(dict["RunAtLoad"].as_boolean(), Some(true));
        set(&dir, Path::new("/x"), false).unwrap();
        assert!(!is_enabled(&dir));
        set(&dir, Path::new("/x"), false).unwrap();
    }

    #[test]
    fn 程序被挪走后开机自启的路径() {
        let t = TempTree::new();
        let dir = t.0.join("LaunchAgents");
        let now = t.0.join("现在的程序");
        std::fs::write(&now, "").unwrap();
        // 记的程序已经不在：不算开着，启动时改成现在的
        set(&dir, &t.0.join("挪走了"), true).unwrap();
        assert!(!is_enabled(&dir));
        repair(&dir, &now).unwrap();
        assert!(is_enabled(&dir));
        // 记的程序还在：不动
        let other = t.0.join("另一份");
        std::fs::write(&other, "").unwrap();
        repair(&dir, &other).unwrap();
        assert_eq!(recorded_exe(&dir), Some(now));
        // 没开自启：不会被打开
        set(&dir, &other, false).unwrap();
        repair(&dir, &other).unwrap();
        assert!(!file(&dir).exists());
        // 临时路径不写进去
        set(&dir, &t.0.join("挪走了"), true).unwrap();
        repair(&dir, Path::new("/private/var/folders/x/AppTranslocation/y/d/Pika Clean.app/Contents/MacOS/pika")).unwrap();
        assert_eq!(recorded_exe(&dir), Some(t.0.join("挪走了")));
        assert!(is_temporary(Path::new("/Volumes/Pika Clean/Pika Clean.app/Contents/MacOS/pika")));
        assert!(!is_temporary(Path::new("/Applications/Pika Clean.app/Contents/MacOS/pika")));
    }

    #[test]
    fn 改名前的启动代理换成现在的() {
        let t = TempTree::new();
        let dir = t.0.join("LaunchAgents");
        migrate_from(&dir, "com.old.name", Path::new("/new")).unwrap();
        assert!(!is_enabled(&dir), "原来没开自启，搬完也不该开");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("com.old.name.plist"), "旧的").unwrap();
        migrate_from(&dir, "com.old.name", Path::new("/new")).unwrap();
        assert!(file(&dir).is_file() && !dir.join("com.old.name.plist").exists());
        assert!(std::fs::read_to_string(file(&dir)).unwrap().contains("<string>/new</string>"));
    }
}
