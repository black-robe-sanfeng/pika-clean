//! 自填 AI 服务的密钥，存在系统钥匙串里（规格不变量 7）。不写进任何文件，也不进日志。

use crate::i18n::t;
use security_framework::item::{ItemClass, ItemSearchOptions};
use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};

const SERVICE: &str = "com.pikaclean.mac.ai-api-key";
const ACCOUNT: &str = "default";

/// 取出密钥。只在真要调用自填服务时用：读密钥内容要过钥匙串的访问授权，应用签名变了（每次更新）可能弹框。
pub fn get() -> Option<String> {
    get_in(SERVICE)
}

/// 存没存密钥。只查条目在不在、不读内容，所以不会弹钥匙串授权框。
pub fn exists() -> bool {
    exists_in(SERVICE)
}

pub fn set(key: &str) -> Result<(), String> {
    set_in(SERVICE, key)
}

pub fn clear() {
    clear_in(SERVICE);
}

/// 把改名前存的密钥搬到现在的条目下，搬完删掉旧的。现在的条目里已经有密钥时只删旧的。
/// 先看旧条目：绝大多数时候它不存在，这样平时启动不会多读一次钥匙串。
pub fn migrate_from(old_service: &str) {
    migrate(old_service, SERVICE);
}

fn migrate(old_service: &str, service: &str) {
    let Some(key) = get_in(old_service) else { return };
    if get_in(service).is_some() || set_in(service, &key).is_ok() {
        clear_in(old_service);
    }
}

fn get_in(service: &str) -> Option<String> {
    get_generic_password(service, ACCOUNT).ok().and_then(|bytes| String::from_utf8(bytes).ok()).filter(|k| !k.is_empty())
}

fn exists_in(service: &str) -> bool {
    ItemSearchOptions::new()
        .class(ItemClass::generic_password())
        .service(service)
        .account(ACCOUNT)
        .load_attributes(true)
        .search()
        .is_ok_and(|found| !found.is_empty())
}

fn set_in(service: &str, key: &str) -> Result<(), String> {
    set_generic_password(service, ACCOUNT, key.as_bytes()).map_err(|e| format!("{}{e}", t("写入钥匙串失败：")))
}

fn clear_in(service: &str) {
    let _ = delete_generic_password(service, ACCOUNT);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用一个测试专用的条目名，不碰真正的密钥。测完删掉。
    #[test]
    fn 密钥能存能取能删() {
        let service = format!("com.pikaclean.mac.test-{}", std::process::id());
        assert_eq!((get_in(&service), exists_in(&service)), (None, false));
        set_in(&service, "sk-第一个").unwrap();
        assert!(exists_in(&service));
        set_in(&service, "sk-第二个").unwrap();
        assert_eq!(get_in(&service).as_deref(), Some("sk-第二个"), "再存一次是覆盖");
        clear_in(&service);
        assert_eq!((get_in(&service), exists_in(&service)), (None, false));
        clear_in(&service);
    }

    #[test]
    fn 旧条目里的密钥搬到新条目() {
        let (old, new) = (format!("com.pikaclean.mac.test-old-{}", std::process::id()), format!("com.pikaclean.mac.test-new-{}", std::process::id()));
        migrate(&old, &new);
        assert_eq!(get_in(&new), None, "没有旧密钥时什么都不做");
        set_in(&old, "sk-旧的").unwrap();
        migrate(&old, &new);
        assert_eq!((get_in(&old), get_in(&new).as_deref()), (None, Some("sk-旧的")));
        // 新条目已经有密钥：保留新的，旧的照样清掉
        set_in(&old, "sk-更旧的").unwrap();
        migrate(&old, &new);
        assert_eq!((get_in(&old), get_in(&new).as_deref()), (None, Some("sk-旧的")));
        clear_in(&new);
    }
}
