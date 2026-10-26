//! 持久化。数据量小，都存成应用数据目录下的文件（规格 T-03）。

use crate::model::{JournalSummary, RemoveResult};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const JOURNAL: &str = "journal.jsonl";
pub const SETTINGS: &str = "settings.json";
pub const AI_CACHE_DIR: &str = "ai-cache";
pub const SPACE: &str = "space.json";
/// 每天一条启动盘已用量，算用量趋势用（规格切片 14）。
pub const USAGE: &str = "usage.jsonl";

/// 程序坞图标显示还是隐藏。
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DockMode {
    /// 默认值，与没有这项设置时的行为一致。
    /// 早先还有过一个「跟随窗口」的选项，已经去掉；存着它的设置文件按「显示」读，否则整份设置都会读不出来
    #[default]
    #[serde(alias = "follow_window")]
    Show,
    Hide,
}

/// 用户设置。文件不存在或损坏时取默认值。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// 用户是否已同意把条目信息发给 AI（规格不变量 4）
    pub ai_consent: bool,
    pub dock: DockMode,
    /// 用户自己填的 AI 服务（OpenAI 兼容格式）。密钥不在这里，在系统钥匙串里。
    pub api: Option<crate::ai::ApiConfig>,
    /// 用户指定用哪个 AI 后端；为空时自动选
    pub ai_backend: Option<crate::ai::Backend>,
    /// 调用 Claude Code 时用的模型和推理强度；不填就用它自己的默认值
    pub claude: crate::ai::ClaudeOptions,
    /// 是否已经看过首次启动的说明
    pub onboarded: bool,
    /// 关掉「磁盘快满时提醒」。默认提醒，所以存的是「关掉」
    pub mute_low_disk_alert: bool,
    /// 界面语言的代号（见 `i18n::Lang::code`）；不填就跟随系统
    pub language: Option<String>,
    /// 用户点了「不再提示」的条目路径：扫描时照样列出，但不再默认勾选
    pub ignored: Vec<String>,
}

pub fn load_settings(dir: &Path) -> Settings {
    let path = dir.join(SETTINGS);
    let Ok(text) = std::fs::read_to_string(&path) else { return Settings::default() };
    serde_json::from_str(&text).unwrap_or_else(|_| {
        // 文件在却读不懂：改名留底再用默认值，免得下一次改设置时拿默认值把它覆盖掉
        let _ = std::fs::rename(&path, dir.join(format!("{SETTINGS}.bad")));
        Settings::default()
    })
}

/// 同一时刻只有一个线程在写临时文件，免得两次保存写进同一个临时文件。
static SETTINGS_WRITE: Mutex<()> = Mutex::new(());

/// 先写临时文件再改名替换：写到一半崩溃或断电，留下的也是完整的旧文件。
pub fn save_settings(dir: &Path, settings: &Settings) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{SETTINGS}.tmp"));
    let _guard = SETTINGS_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    std::fs::write(&tmp, serde_json::to_string_pretty(settings)?)?;
    std::fs::rename(tmp, dir.join(SETTINGS))
}

/// 最近一次空间统计。重启后打开「空间去向」直接显示它，不用重新数。文件不存在或读不懂时为空。
pub fn load_space(dir: &Path) -> Option<crate::space::SpaceReport> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(SPACE)).ok()?).ok()
}

pub fn save_space(dir: &Path, report: &crate::space::SpaceReport) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(SPACE), serde_json::to_string(report)?)
}

/// 最近一次清理移到废纸篓的条目。只留最近一批（下一次有东西进废纸篓时覆盖），不长期攒着用户的文件路径。
pub const LAST_TRASHED: &str = "last-trashed.json";

pub fn load_trashed(dir: &Path) -> Vec<crate::model::Trashed> {
    std::fs::read_to_string(dir.join(LAST_TRASHED)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save_trashed(dir: &Path, list: &[crate::model::Trashed]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(LAST_TRASHED), serde_json::to_string(list)?)
}

/// 可用空间低于这个比例时提醒（规格 O-05 的推荐值）。
pub const LOW_DISK_FREE_RATIO: f64 = 0.10;

/// 现在该不该提醒「磁盘快满了」：可用空间不足一成、用户没关提醒、今天还没提醒过。
pub fn should_alert_low_disk(used: u64, total: u64, muted: bool, last_alert_day: Option<u64>, today: u64) -> bool {
    let low = total > 0 && (total.saturating_sub(used) as f64) < total as f64 * LOW_DISK_FREE_RATIO;
    low && !muted && last_alert_day != Some(today)
}

/// 几个命令可能同时写清理记录（比如连点两下）：串行写，免得两行交错写坏。
static JOURNAL_WRITE: Mutex<()> = Mutex::new(());

fn append(dir: &Path, mut entry: serde_json::Value) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    entry["ts_ms"] = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0).into();
    // 整行拼好再一次写入：直接 writeln! 会一个记号一次系统调用
    let line = format!("{entry}\n");
    let _guard = JOURNAL_WRITE.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = std::fs::File::options().create(true).append(true).open(dir.join(JOURNAL))?;
    f.write_all(line.as_bytes())
}

/// 往清理记录里追加一条清理：一行一个 JSON。
pub fn append_journal(dir: &Path, result: &RemoveResult) -> std::io::Result<()> {
    append(dir, serde_json::json!({ "kind": "clean", "freed_bytes": result.freed_bytes, "trashed_bytes": result.trashed_bytes, "failed": result.failed.len() }))
}

/// 记一次卸载：卸了几个应用，连同残留文件一共多大。
pub fn append_uninstall(dir: &Path, apps: u64, result: &RemoveResult) -> std::io::Result<()> {
    append(dir, serde_json::json!({ "kind": "uninstall", "apps": apps, "trashed_bytes": result.trashed_bytes, "failed": result.failed.len() }))
}

/// 记一次优化操作（退出进程、运行维护任务等）。
pub fn append_optimize(dir: &Path, what: &str) -> std::io::Result<()> {
    append(dir, serde_json::json!({ "kind": "optimize", "what": what }))
}

/// 把清理记录汇总成三个数。记录文件不存在或某一行损坏都不算错。
pub fn journal_summary(dir: &Path) -> JournalSummary {
    let mut sum = JournalSummary::default();
    let text = std::fs::read_to_string(dir.join(JOURNAL)).unwrap_or_default();
    for entry in text.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
        let n = |k: &str| entry[k].as_u64().unwrap_or(0);
        match entry["kind"].as_str().unwrap_or("clean") {
            "uninstall" => sum.uninstalled += n("apps"),
            "optimize" => sum.optimized += 1,
            _ => sum.cleaned_bytes += n("freed_bytes") + n("trashed_bytes"),
        }
    }
    sum
}

/// 某一天（自 1970 年起的第几天，UTC）的启动盘已用量和总容量。
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct UsagePoint {
    pub day: u64,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

/// 趋势只看最近这么多天。
pub const TREND_DAYS: u64 = 28;

/// 用量趋势：最近 28 天每天的已用量，首尾两点的斜率，以及照这个速度几天后写满。
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct UsageTrend {
    pub points: Vec<UsagePoint>,
    /// 每天多用（负数是少用）多少字节
    pub bytes_per_day: i64,
    /// 用量在涨时，照这个速度还有几天写满；不在涨时为空
    pub days_to_full: Option<u64>,
}

/// 记下今天的已用量。今天已经记过（比如当天重启了应用）就不再记。
pub fn record_usage(dir: &Path, day: u64, used_bytes: u64, total_bytes: u64) -> std::io::Result<()> {
    if load_usage(dir).last().is_some_and(|p| p.day >= day) {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    let line = format!("{}\n", serde_json::to_string(&UsagePoint { day, used_bytes, total_bytes })?);
    std::fs::File::options().create(true).append(true).open(dir.join(USAGE))?.write_all(line.as_bytes())
}

/// 记过的全部用量，按记录顺序。文件不存在或某一行读不懂都不算错。
pub fn load_usage(dir: &Path) -> Vec<UsagePoint> {
    std::fs::read_to_string(dir.join(USAGE)).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// 取最后一条往前 28 天内的记录，用首尾两点算斜率。不足两天不下结论；用量没在涨就不预测几天后满。
pub fn usage_trend(all: &[UsagePoint]) -> Option<UsageTrend> {
    let last = *all.last()?;
    let points: Vec<UsagePoint> = all.iter().copied().filter(|p| p.day + TREND_DAYS > last.day && p.day <= last.day).collect();
    let first = points[0];
    if last.day <= first.day {
        return None;
    }
    let bytes_per_day = ((last.used_bytes as f64 - first.used_bytes as f64) / (last.day - first.day) as f64) as i64;
    let days_to_full = (bytes_per_day > 0).then(|| last.total_bytes.saturating_sub(last.used_bytes) / bytes_per_day as u64);
    Some(UsageTrend { points, bytes_per_day, days_to_full })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::testutil::TempTree;

    #[test]
    fn 设置能存能取且损坏时回到默认值() {
        let t = TempTree::new();
        assert_eq!(load_settings(&t.0), Settings::default());
        save_settings(&t.0, &Settings { ai_consent: true, dock: DockMode::Hide, ..Default::default() }).unwrap();
        assert_eq!(load_settings(&t.0), Settings { ai_consent: true, dock: DockMode::Hide, ..Default::default() });
        // 已经去掉的「跟随窗口」按「显示」读，其它设置不受影响
        std::fs::write(t.0.join(SETTINGS), r#"{"ai_consent":true,"dock":"follow_window"}"#).unwrap();
        assert_eq!(load_settings(&t.0), Settings { ai_consent: true, dock: DockMode::Show, ..Default::default() });
        // 旧版本写的设置文件里没有 dock 字段，要能照常读出来
        std::fs::write(t.0.join(SETTINGS), r#"{"ai_consent":true}"#).unwrap();
        assert_eq!(load_settings(&t.0), Settings { ai_consent: true, dock: DockMode::Show, ..Default::default() });
        std::fs::write(t.0.join(SETTINGS), "{坏的").unwrap();
        assert_eq!(load_settings(&t.0), Settings::default());
        // 读不懂的文件改名留底，不会被之后的保存覆盖掉
        assert_eq!(std::fs::read_to_string(t.0.join("settings.json.bad")).unwrap(), "{坏的");
        save_settings(&t.0, &Settings { onboarded: true, ..Default::default() }).unwrap();
        assert!(load_settings(&t.0).onboarded && !t.0.join("settings.json.tmp").exists());
    }

    #[test]
    fn 磁盘快满的提醒条件() {
        assert!(should_alert_low_disk(95, 100, false, None, 7));
        assert!(should_alert_low_disk(95, 100, false, Some(6), 7));
        assert!(!should_alert_low_disk(95, 100, false, Some(7), 7), "一天只提醒一次");
        assert!(!should_alert_low_disk(95, 100, true, None, 7), "用户关掉了提醒");
        assert!(!should_alert_low_disk(85, 100, false, None, 7), "还有一成以上的空间");
        assert!(!should_alert_low_disk(0, 0, false, None, 7));
    }

    #[test]
    fn 清理记录逐条追加() {
        let t = TempTree::new();
        let r = RemoveResult { freed_bytes: 5, trashed_bytes: 3, ..Default::default() };
        append_journal(&t.0, &r).unwrap();
        append_journal(&t.0, &r).unwrap();
        let text = std::fs::read_to_string(t.0.join(JOURNAL)).unwrap();
        let lines: Vec<serde_json::Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!((lines[1]["freed_bytes"].as_u64(), lines[1]["trashed_bytes"].as_u64()), (Some(5), Some(3)));

        append_uninstall(&t.0, 2, &r).unwrap();
        append_optimize(&t.0, "quit").unwrap();
        std::fs::write(t.0.join(JOURNAL), std::fs::read_to_string(t.0.join(JOURNAL)).unwrap() + "坏掉的一行\n").unwrap();
        assert_eq!(journal_summary(&t.0), JournalSummary { cleaned_bytes: 16, uninstalled: 2, optimized: 1 });
        assert_eq!(journal_summary(&t.0.join("不存在")), JournalSummary::default());
    }

    #[test]
    fn 同时写清理记录不会交错() {
        let t = TempTree::new();
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| (0..50).for_each(|_| append_optimize(&t.0, "admin").unwrap()));
            }
        });
        assert_eq!(journal_summary(&t.0).optimized, 400);
    }

    #[test]
    fn 用量趋势() {
        let p = |day, gb: u64| UsagePoint { day, used_bytes: gb * 1_000_000_000, total_bytes: 500_000_000_000 };
        let t = TempTree::new();
        record_usage(&t.0, 10, 100_000_000_000, 500_000_000_000).unwrap();
        record_usage(&t.0, 10, 999, 500_000_000_000).unwrap();
        assert_eq!(load_usage(&t.0), [p(10, 100)], "同一天只记一次");
        assert_eq!(usage_trend(&load_usage(&t.0)), None, "不足两天不下结论");
        assert_eq!(usage_trend(&[]), None);

        // 只看最近 28 天：第 0 天那条不算，斜率按第 10 天到第 30 天算
        let trend = usage_trend(&[p(0, 10), p(10, 100), p(20, 105), p(30, 120)]).unwrap();
        assert_eq!(trend.points.len(), 3);
        assert_eq!(trend.bytes_per_day, 1_000_000_000);
        assert_eq!(trend.days_to_full, Some(380));

        let down = usage_trend(&[p(1, 120), p(5, 100)]).unwrap();
        assert!(down.bytes_per_day < 0);
        assert_eq!(down.days_to_full, None, "用量在降时不预测");
    }
}
