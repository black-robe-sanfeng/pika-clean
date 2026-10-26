//! 2026-10-14 产品从「小毛头清理大师」改名为 Pika Clean，应用标识也跟着换了。
//! 这里把旧标识下的东西搬到新标识下：数据目录（设置、清理记录、用量历史、AI 缓存）、
//! 钥匙串里的 API Key、开机自启。每次启动都会走到这里，搬过了就什么都不做。
//!
//! 搬不了的：完全磁盘访问、通知这些系统权限是系统按标识记的，改名后需要用户重新授权。

use std::path::Path;
use tauri::{AppHandle, Manager};

/// 改名前的应用标识。只在本模块里出现。
const OLD_ID: &str = "com.xiaomaotou.cleaner";

/// 旧数据目录还在、新的还没有，就把旧的整个改名过来。返回有没有搬。
/// 新目录已经存在时不动：那说明新版已经用过，合并两份数据比丢一份旧的更容易出错。
pub fn data_dir(new: &Path) -> std::io::Result<bool> {
    let old = new.with_file_name(OLD_ID);
    if new.exists() || !old.is_dir() {
        return Ok(false);
    }
    std::fs::rename(old, new)?;
    Ok(true)
}

pub fn run(app: &AppHandle) {
    // 自测用的是另一个标识和另一个数据目录，不能把真实数据搬进去
    if app.config().identifier != crate::autostart::LABEL {
        return;
    }
    if let Ok(dir) = app.path().app_data_dir() {
        if let Err(e) = data_dir(&dir) {
            eprintln!("搬旧数据目录失败：{e}");
        }
    }
    crate::keychain::migrate_from(&format!("{OLD_ID}.ai-api-key"));
    if let (Some(home), Ok(exe)) = (std::env::var_os("HOME"), std::env::current_exe()) {
        if let Err(e) = crate::autostart::migrate_from(&Path::new(&home).join("Library/LaunchAgents"), OLD_ID, &exe) {
            eprintln!("搬开机自启失败：{e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::testutil::TempTree;

    #[test]
    fn 旧数据目录整个搬到新标识下且只搬一次() {
        let t = TempTree::new();
        let (old, new) = (t.0.join(OLD_ID), t.0.join("com.pikaclean.mac"));
        std::fs::create_dir_all(old.join("ai-cache")).unwrap();
        std::fs::write(old.join("settings.json"), r#"{"ai_consent":true}"#).unwrap();
        std::fs::write(old.join("ai-cache/a.json"), "{}").unwrap();

        assert!(data_dir(&new).unwrap());
        assert!(!old.exists());
        assert!(crate::db::load_settings(&new).ai_consent);
        assert!(new.join("ai-cache/a.json").is_file());
        assert!(!data_dir(&new).unwrap(), "搬过了就不再动");

        // 新目录已经在用时，后来又冒出来的旧目录不能盖掉它
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.json"), r#"{"ai_consent":false}"#).unwrap();
        assert!(!data_dir(&new).unwrap());
        assert!(crate::db::load_settings(&new).ai_consent);
        assert!(old.is_dir());
    }
}
