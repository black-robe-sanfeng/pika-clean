//! 扩展和插件：快速查看插件、偏好设置面板、输入法、屏幕保护程序、网页插件，以及 Chrome、Edge、Brave 的扩展。
//! 用户资源库里的可以移到废纸篓；`/Library` 里的要管理员权限，只在访达里显示；浏览器扩展要在浏览器里删
//! （直接删文件夹浏览器会报「扩展已损坏」）。

use std::path::{Path, PathBuf};

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct Plugin {
    /// 也当编号用
    pub path: String,
    pub name: String,
    /// quicklook / prefpane / input / saver / internet / browser
    pub kind: &'static str,
    /// 浏览器扩展属于哪个浏览器（应用名）
    pub browser: Option<&'static str>,
    /// 能直接移到废纸篓（在用户资源库里，不是浏览器扩展）
    pub removable: bool,
}

/// 资源库下的目录、插件包的扩展名、种类。
const KINDS: [(&str, &str, &str); 5] = [
    ("QuickLook", "qlgenerator", "quicklook"),
    ("PreferencePanes", "prefPane", "prefpane"),
    ("Input Methods", "app", "input"),
    ("Screen Savers", "saver", "saver"),
    ("Internet Plug-Ins", "plugin", "internet"),
];

/// 浏览器扩展目录（相对主目录）和浏览器的应用名。只看默认的那个用户。
// ponytail: 浏览器里另建的用户（Profile 1 …）不看，要的话再按 Local State 里的列表遍历
const BROWSERS: [(&str, &str); 3] = [
    ("Library/Application Support/Google/Chrome/Default/Extensions", "Google Chrome"),
    ("Library/Application Support/Microsoft Edge/Default/Extensions", "Microsoft Edge"),
    ("Library/Application Support/BraveSoftware/Brave-Browser/Default/Extensions", "Brave Browser"),
];

fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(dir).map(|e| e.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    out.sort();
    out
}

/// 插件包自己写的名字，没有就用文件名。
fn bundle_name(p: &Path) -> String {
    let dict = plist::Value::from_file(p.join("Contents/Info.plist")).ok().and_then(|v| v.into_dictionary());
    let get = |k: &str| dict.as_ref()?.get(k)?.as_string().map(str::to_owned).filter(|s| !s.trim().is_empty());
    get("CFBundleDisplayName").or_else(|| get("CFBundleName")).unwrap_or_else(|| p.file_stem().unwrap_or_default().to_string_lossy().into_owned())
}

/// 浏览器扩展的名字。manifest 里写成 `__MSG_appName__` 的，去默认语言的 messages.json 里查（键不分大小写）。
fn extension_name(version_dir: &Path) -> Option<String> {
    let read = |p: PathBuf| -> Option<serde_json::Value> { serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok() };
    let manifest = read(version_dir.join("manifest.json"))?;
    let name = manifest.get("name")?.as_str()?;
    let Some(key) = name.strip_prefix("__MSG_").and_then(|k| k.strip_suffix("__")) else { return Some(name.to_owned()) };
    let locale = manifest.get("default_locale").and_then(|v| v.as_str()).unwrap_or("en");
    let messages = read(version_dir.join("_locales").join(locale).join("messages.json"))?;
    let found = messages.as_object()?.iter().find(|(k, _)| k.eq_ignore_ascii_case(key))?;
    found.1.get("message")?.as_str().map(str::to_owned)
}

/// 本机装着的扩展和插件，先按种类、再按名字排。
pub fn list(home: &Path, system_library: &Path) -> Vec<Plugin> {
    let mut out = Vec::new();
    for (library, removable) in [(home.join("Library"), true), (system_library.to_path_buf(), false)] {
        for (dir, ext, kind) in KINDS {
            for p in entries(&library.join(dir)).into_iter().filter(|p| p.extension().is_some_and(|x| x == ext)) {
                // 链接不跟，免得移走的是别处的东西
                if p.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                    out.push(Plugin { name: bundle_name(&p), path: p.to_string_lossy().into_owned(), kind, browser: None, removable });
                }
            }
        }
    }
    for (dir, browser) in BROWSERS {
        // 扩展编号是 32 个 a–p 的字母；目录里还有 Temp 之类的
        for p in entries(&home.join(dir)).into_iter().filter(|p| p.file_name().is_some_and(|n| n.len() == 32 && n.to_string_lossy().bytes().all(|b| (b'a'..=b'p').contains(&b)))) {
            if let Some(name) = entries(&p).last().and_then(|v| extension_name(v)) {
                out.push(Plugin { name, path: p.to_string_lossy().into_owned(), kind: "browser", browser: Some(browser), removable: false });
            }
        }
    }
    out.sort_by(|a, b| (a.kind, a.name.to_lowercase()).cmp(&(b.kind, b.name.to_lowercase())));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::testutil::TempTree;

    #[test]
    fn 列出插件和浏览器扩展() {
        let t = TempTree::new();
        t.file("Library/QuickLook/Markdown.qlgenerator/Contents/Info.plist", 0);
        std::fs::write(
            t.0.join("Library/QuickLook/Markdown.qlgenerator/Contents/Info.plist"),
            r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleName</key><string>QLMarkdown</string></dict></plist>"#,
        )
        .unwrap();
        t.file("Library/QuickLook/readme.txt", 1);
        t.file("Sys/PreferencePanes/Flash.prefPane/Contents/x", 1);
        let ext = "Library/Application Support/Google/Chrome/Default/Extensions/aapocclcgogkmnckokdopfmhonfmgoek/1.0_0";
        t.file(&format!("{ext}/manifest.json"), 0);
        std::fs::write(t.0.join(ext).join("manifest.json"), r#"{"name":"__MSG_appName__","default_locale":"en"}"#).unwrap();
        t.file(&format!("{ext}/_locales/en/messages.json"), 0);
        std::fs::write(t.0.join(ext).join("_locales/en/messages.json"), r#"{"APPNAME":{"message":"Slides"}}"#).unwrap();
        t.file("Library/Application Support/Google/Chrome/Default/Extensions/Temp/x", 1);

        let got: Vec<_> = list(&t.0, &t.0.join("Sys")).into_iter().map(|p| (p.kind, p.name, p.browser, p.removable)).collect();
        assert_eq!(
            got,
            vec![
                ("browser", "Slides".into(), Some("Google Chrome"), false),
                ("prefpane", "Flash".into(), None, false),
                ("quicklook", "QLMarkdown".into(), None, true),
            ]
        );
    }
}

#[cfg(test)]
mod real {
    #[test]
    #[ignore = "看本机真实装着的"]
    fn 本机的扩展和插件() {
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        for p in super::list(&home, std::path::Path::new("/Library")) {
            eprintln!("{:10} {:6} {} ({:?})", p.kind, p.removable, p.name, p.browser);
        }
    }
}
