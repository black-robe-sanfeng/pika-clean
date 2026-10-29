//! 已装的应用有没有新版本。只看用 Sparkle 自动更新的应用：读它自己写在 Info.plist 里的更新源（SUFeedURL），
//! 取里面最新的正式版，和装着的版本比。只在用户点「检查更新」时联网，只连应用自己写明的 https 地址。
//! 再加上 Homebrew 装的图形应用（cask）：问 `brew outdated`，升级要用户自己在终端里跑。
// ponytail: 不查 App Store（它自己会提示）和 Homebrew 的命令行工具（大多是被依赖的库，列出来太乱）

use crate::model::AppInfo;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct AppUpdate {
    pub id: String,
    pub name: String,
    pub current: String,
    pub latest: String,
    /// Homebrew 装的：升级用的命令
    pub brew: Option<String>,
}

/// 一个更新源最多等这么久。
const FEED_TIMEOUT: Duration = Duration::from_secs(8);
const WORKERS: usize = 8;

/// 应用的更新源地址和装着的构建号。不是 https 的不认。
fn feed_of(app: &Path) -> Option<(String, String)> {
    let dict = plist::Value::from_file(app.join("Contents/Info.plist")).ok()?.into_dictionary()?;
    let url = dict.get("SUFeedURL")?.as_string()?.trim().to_owned();
    let build = dict.get("CFBundleVersion").and_then(|v| v.as_string()).unwrap_or_default().to_owned();
    url.starts_with("https://").then_some((url, build))
}

/// 版本号里的数字段，「1.10.2 (b3)」→ [1, 10, 2, 3]。比较时逐段比数字，不按字符串比。
fn parts(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect()
}

fn newer(a: &str, b: &str) -> bool {
    parts(a) > parts(b)
}

/// 取一个 `<item>` 里某个字段：可能写成属性 `sparkle:version="…"`，也可能写成元素 `<sparkle:version>…</…>`。
fn field(item: &str, name: &str) -> Option<String> {
    let attr = format!("sparkle:{name}=\"");
    let elem = format!("<sparkle:{name}>");
    let v = if let Some(i) = item.find(&attr) {
        let rest = &item[i + attr.len()..];
        &rest[..rest.find('"')?]
    } else {
        let rest = &item[item.find(&elem)? + elem.len()..];
        &rest[..rest.find('<')?]
    };
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_owned())
}

/// 更新源里最新的正式版：（显示的版本号，构建号）。带 `sparkle:channel` 的是测试版，跳过。
pub(crate) fn latest_in(xml: &str) -> Option<(Option<String>, Option<String>)> {
    xml.split("<item")
        .skip(1)
        .filter(|item| !item.contains("<sparkle:channel>"))
        .map(|item| (field(item, "shortVersionString"), field(item, "version")))
        .filter(|(s, b)| s.is_some() || b.is_some())
        .max_by(|x, y| {
            let key = |(s, b): &(Option<String>, Option<String>)| parts(b.as_deref().or(s.as_deref()).unwrap_or(""));
            key(x).cmp(&key(y))
        })
}

/// 装着的版本（显示的版本号、构建号）和更新源里最新的比，新的话返回要显示的最新版本号。
/// 两边都有显示的版本号就比它，否则比构建号。
pub(crate) fn update_for(current: &str, build: &str, latest: (Option<String>, Option<String>)) -> Option<String> {
    match latest {
        (Some(short), _) if !current.is_empty() => newer(&short, current).then_some(short),
        (short, Some(b)) => newer(&b, build).then(|| short.unwrap_or(b)),
        _ => None,
    }
}

fn check_one(app: &AppInfo) -> Option<AppUpdate> {
    let (url, build) = feed_of(&app.path)?;
    let xml = ureq::get(&url).timeout(FEED_TIMEOUT).call().ok()?.into_string().ok()?;
    let latest = update_for(&app.version, &build, latest_in(&xml)?)?;
    Some(AppUpdate { id: app.id.clone(), name: app.name.clone(), current: app.version.clone(), latest, brew: None })
}

/// `brew outdated --json=v2 --cask` 的结果里，固定了版本（pinned）的不算。
/// 不加 `--greedy`：自己会自动更新的应用 brew 本来就不管，交给上面的 Sparkle 检查。
pub(crate) fn casks_in(json: &str) -> Vec<AppUpdate> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return vec![] };
    let casks = v.get("casks").and_then(|c| c.as_array()).cloned().unwrap_or_default();
    casks
        .iter()
        .filter(|c| !c.get("pinned").and_then(|p| p.as_bool()).unwrap_or(false))
        .filter_map(|c| {
            let name = c.get("name")?.as_str()?;
            let current = c.get("installed_versions")?.as_array()?.last()?.as_str()?;
            let latest = c.get("current_version")?.as_str()?;
            Some(AppUpdate { id: format!("brew:{name}"), name: name.into(), current: current.into(), latest: latest.into(), brew: Some(format!("brew upgrade --cask {name}")) })
        })
        .collect()
}

/// 没装 Homebrew 或者跑不起来就是空的。
fn brew_casks() -> Vec<AppUpdate> {
    let Some(brew) = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"].into_iter().find(|p| Path::new(p).exists()) else { return vec![] };
    let out = std::process::Command::new(brew).args(["outdated", "--json=v2", "--cask"]).stderr(std::process::Stdio::null()).output();
    out.map(|o| casks_in(&String::from_utf8_lossy(&o.stdout))).unwrap_or_default()
}

/// 并发查一遍，按名字排序。连不上、读不懂的更新源直接跳过。
pub fn check(apps: &[AppInfo]) -> Vec<AppUpdate> {
    let next = AtomicUsize::new(0);
    let mut out: Vec<AppUpdate> = std::thread::scope(|s| {
        let brew = s.spawn(brew_casks);
        let workers: Vec<_> = (0..WORKERS)
            .map(|_| {
                s.spawn(|| {
                    let mut mine = Vec::new();
                    while let Some(app) = apps.get(next.fetch_add(1, Ordering::Relaxed)) {
                        mine.extend(check_one(app));
                    }
                    mine
                })
            })
            .collect();
        let mut all: Vec<AppUpdate> = workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect();
        all.extend(brew.join().unwrap_or_default());
        all
    });
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 读更新源里最新的正式版并和装着的比() {
        let xml = r#"<rss><channel>
            <item><title>2.1</title><enclosure url="x" sparkle:version="210" sparkle:shortVersionString="2.1"/></item>
            <item><title>2.10</title><sparkle:version>2100</sparkle:version><sparkle:shortVersionString>2.10</sparkle:shortVersionString></item>
            <item><sparkle:channel>beta</sparkle:channel><enclosure sparkle:version="9000" sparkle:shortVersionString="3.0b1"/></item>
        </channel></rss>"#;
        let latest = latest_in(xml).unwrap();
        assert_eq!(latest, (Some("2.10".into()), Some("2100".into())), "2.10 比 2.1 新，测试版不算");
        assert_eq!(update_for("2.9", "290", latest.clone()), Some("2.10".into()));
        assert_eq!(update_for("2.10", "2100", latest), None, "已经是最新");
        // 只有构建号的更新源
        assert_eq!(update_for("", "41", (None, Some("42".into()))), Some("42".into()));
        assert_eq!(latest_in("<rss></rss>"), None);
    }

    #[test]
    fn 读_brew_过期的图形应用() {
        let json = r#"{"formulae":[{"name":"abseil","installed_versions":["1"],"current_version":"2","pinned":false}],
            "casks":[{"name":"calibre","installed_versions":["7.16.0"],"current_version":"9.17.0","pinned":false},
                     {"name":"old","installed_versions":["1"],"current_version":"2","pinned":true}]}"#;
        let got = casks_in(json);
        assert_eq!(got.len(), 1, "命令行工具和固定了版本的不算");
        assert_eq!((got[0].current.as_str(), got[0].latest.as_str(), got[0].brew.as_deref()), ("7.16.0", "9.17.0", Some("brew upgrade --cask calibre")));
        assert!(casks_in("not json").is_empty());
    }

    #[test]
    #[ignore = "要联网，查本机真实装着的应用"]
    fn 查本机应用的更新() {
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        let apps = crate::apps::list(&crate::apps::default_roots(&home), &|_| Default::default(), &|_| (false, vec![]));
        let with_feed = apps.iter().filter(|a| feed_of(&a.path).is_some()).count();
        let found = check(&apps);
        eprintln!("{} 个应用，{with_feed} 个有更新源，{} 个有新版本：{found:#?}", apps.len(), found.len());
    }
}
