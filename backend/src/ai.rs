//! AI 调用的唯一入口。
//!
//! - AI 只返回文字。调用命令行时关闭它的全部工具，并在空的临时目录里运行。
//! - 发给 AI 的内容由本模块从后端自己的数据里拼出来，前端只传条目编号或进程名，
//!   所以「只发名称、路径、大小、时间」是结构上保证的，不靠调用方自觉。

use crate::i18n::t;
use crate::model::{AppInfo, Category, Item, Proc, ScanReport, Snapshot};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const TIMEOUT: Duration = Duration::from_secs(60);
const CACHE_TTL: Duration = Duration::from_secs(30 * 86400);
/// 用什么语言回答由 `Lang::ai_instruction` 接在后面，这里不写死。
const SYSTEM: &str = "你是 macOS 清理软件 Pika Clean 里的解释助手。直接给结论，不要寒暄，不要使用 Markdown 标记。你没有任何工具可用，也不要建议用户去运行命令。";

/// 两种后端：本机的 Claude Code，或用户自己填的 AI 服务。
/// Codex 在探路中被判定不能用（关不掉它的改文件工具）。
#[derive(Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    ClaudeCli,
    ApiKey,
}

/// 用户自己填的 AI 服务，OpenAI 兼容格式（规格 O-07 的推荐方案，用户尚未确认）。
#[derive(Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct ApiConfig {
    pub base_url: String,
    pub model: String,
}

/// 推理强度，和 Claude Code 的 `--effort` 取值一一对应。
#[derive(Serialize, serde::Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    fn as_arg(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// 调用 Claude Code 时用户可以指定的两项。不填就用 Claude Code 自己的默认值。
#[derive(Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct ClaudeOptions {
    /// 模型名或别名，如 `sonnet`、`opus`
    pub model: Option<String>,
    pub effort: Option<Effort>,
}

/// 模型名会原样作为命令行参数传给 Claude Code，所以只接受名字里会出现的字符，
/// 而且不能以 `-` 开头，否则会被当成另一个参数。
pub fn valid_model(name: &str) -> bool {
    name.len() <= 64
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || "._-:[]".contains(c))
}

/// 这一次提问可以用哪些后端。由调用方从设置里读出来。
#[derive(Default, Clone)]
pub struct Choice {
    /// 用户指定的后端；None 表示自动：先 Claude Code，再自填的服务
    pub preferred: Option<Backend>,
    /// 自填服务的配置；钥匙串里没有密钥就是 None。密钥本身等真要调用时才读
    pub api: Option<ApiConfig>,
    pub claude: ClaudeOptions,
}

impl Choice {
    pub fn available(&self) -> Vec<Backend> {
        let mut out: Vec<Backend> = claude_path().map(|_| Backend::ClaudeCli).into_iter().collect();
        out.extend(self.api.as_ref().map(|_| Backend::ApiKey));
        out
    }

    fn pick(&self) -> Option<Backend> {
        let all = self.available();
        self.preferred.filter(|p| all.contains(p)).or(all.first().copied())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    ExplainItem,
    ExplainProcess,
    SummarizeScan,
    UninstallRisk,
    Diagnose,
    Plan,
    OrganizeDownloads,
    ExplainSpace,
    ExplainTrend,
    ExplainLoginItem,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum AiError {
    /// 用户还没同意把信息发给 AI
    ConsentRequired,
    NoBackend,
    NotLoggedIn,
    Timeout,
    BadOutput(String),
    Failed(String),
    /// 前端已经不要这个回答了（界面关了或重新提问了）
    Cancelled,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Answer {
    pub text: String,
    pub cached: bool,
    pub backend: Backend,
}

// ---- 定位命令行 ----

/// 图形界面应用的 PATH 里通常没有用户的安装目录，所以三种办法依次试。
pub(crate) fn locate(name: &str) -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let in_path = || std::env::var("PATH").ok()?.split(':').map(|d| Path::new(d).join(name)).find(|p| p.is_file());
    let known = || known_dirs(&home).into_iter().map(|d| d.join(name)).find(|p| p.is_file());
    let login_shell = || {
        let out = Command::new("/bin/zsh").args(["-lc", &format!("command -v {name}")]).output().ok()?;
        let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        path.starts_with('/').then(|| PathBuf::from(path))
    };
    in_path().or_else(known).or_else(login_shell)
}

/// 常见的安装目录。nvm 每个 Node 版本一个目录，逐个展开（它的 PATH 通常写在 .zshrc 里，登录 shell 读不到）。
fn known_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = vec![home.join(".local/bin"), "/opt/homebrew/bin".into(), "/usr/local/bin".into(), home.join(".npm-global/bin"), home.join(".bun/bin"), home.join(".volta/bin")];
    let mut nvm: Vec<PathBuf> = std::fs::read_dir(home.join(".nvm/versions/node")).into_iter().flatten().filter_map(|e| Some(e.ok()?.path().join("bin"))).collect();
    // 版本号大的排前面，多半是用户在用的
    nvm.sort_by(|a, b| b.cmp(a));
    dirs.extend(nvm);
    dirs
}

/// 没找到时过多久再找一次。应用常驻菜单栏，用户中途装好 Claude Code 不用重启就能用上。
const RELOCATE_AFTER: Duration = Duration::from_secs(30);

/// 找到的路径一直记着，用之前确认它还在；没找到的只记 `RELOCATE_AFTER` 这么久。
fn claude_path() -> Option<PathBuf> {
    static CACHE: Mutex<Option<(Option<PathBuf>, Instant)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap();
    match &*cache {
        Some((Some(p), _)) if p.is_file() => return Some(p.clone()),
        Some((None, at)) if at.elapsed() < RELOCATE_AFTER => return None,
        _ => {}
    }
    let found = locate("claude");
    *cache = Some((found.clone(), Instant::now()));
    found
}

/// 前端不再需要的回答（界面关了、重新提问了），按提问编号记下；生成它的那次调用看到后就停下。
/// 编号是前端通道的编号（随机数）。提问结束时把自己的编号删掉。
static CANCELLED: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

pub fn cancel(id: u32) {
    CANCELLED.lock().unwrap().get_or_insert_default().insert(id);
}

fn is_cancelled(id: u32) -> bool {
    CANCELLED.lock().unwrap().as_ref().is_some_and(|s| s.contains(&id))
}



// ---- 发给 AI 的内容 ----

/// 把用户主目录换成 ~，不把用户名发出去。
fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// 和界面上一样的大小写法：磁盘和文件按 1000 进位（同 Finder），内存按 1024（同活动监视器）。
/// 大小由应用换算好再发给 AI，不让它自己从字节数换算，免得说出来的数字和界面对不上（截图评审发现）。
fn human(bytes: u64) -> String {
    scaled(bytes, 1000.0)
}

fn human_mem(bytes: u64) -> String {
    scaled(bytes, 1024.0)
}

fn scaled(bytes: u64, k: f64) -> String {
    let (mb, gb) = (k * k, k * k * k);
    let b = bytes as f64;
    if b >= 100.0 * gb {
        format!("{:.0} GB", b / gb)
    } else if b >= 1000.0 * mb {
        format!("{:.1} GB", b / gb)
    } else if b >= mb {
        format!("{:.0} MB", b / mb)
    } else {
        format!("{:.0} KB", b / k)
    }
}

fn days_ago(ms: Option<u64>, now_ms: u64) -> Value {
    ms.map_or(Value::Null, |t| json!(now_ms.saturating_sub(t) / 86_400_000))
}

pub fn input_for_item(cat: &Category, item: &Item, home: &Path, now_ms: u64) -> Value {
    json!({
        "path": tilde(&item.path, home),
        "size": human(item.size_bytes),
        "category": cat.id.as_str(),
        "last_used_days_ago": days_ago(item.last_used_ms, now_ms),
    })
}

pub fn input_for_process(p: &Proc) -> Value {
    json!({ "name": p.name, "memory": human_mem(p.mem_bytes), "cpu_pct": (p.cpu_pct * 10.0).round() / 10.0 })
}

/// 空间去向里的一个文件夹或文件：只发路径、大小和是文件还是文件夹（不变量 4）。
pub fn input_for_space(node: &crate::space::Node, home: &Path) -> Value {
    json!({ "path": tilde(Path::new(&node.path), home), "size": human(node.size_bytes), "kind": if node.is_dir { "folder" } else { "file" } })
}

/// 一个开机启动项：只发名称、种类、要运行的程序和它还在不在（不变量 4）。登录应用的程序就是应用本身
pub fn input_for_login_item(item: &crate::optimize::LoginItem, home: &Path) -> Value {
    let program = if item.kind == "login_item" { Some(&item.path) } else { item.program.as_ref() };
    json!({ "name": item.name, "kind": item.kind, "program": program.map(|p| tilde(p, home)), "program_missing": item.broken })
}

pub fn input_for_scan(report: &ScanReport, home: &Path) -> Value {
    let cats: Vec<Value> = report
        .categories
        .iter()
        .filter(|c| c.total_bytes > 0)
        .map(|c| {
            json!({
                "category": c.id.as_str(),
                "total": human(c.total_bytes),
                "selected_by_default": c.default_selected && !c.needs_admin && !c.unreadable,
                "largest": c.items.iter().take(3).map(|i| json!({ "path": tilde(&i.path, home), "size": human(i.size_bytes) })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "categories": cats })
}

/// 卸载前的提醒：只发应用名、包标识、多久没用，以及它在哪几类目录里留了数据（目录类别名，不是文件名）。
pub fn input_for_app(app: &AppInfo, now_ms: u64) -> Value {
    let mut kinds: Vec<String> = app
        .leftovers
        .iter()
        .filter_map(|l| l.path.parent()?.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    kinds.sort();
    kinds.dedup();
    json!({
        "name": app.name,
        "bundle_id": app.bundle_id,
        "last_used_days_ago": days_ago(app.last_used_ms, now_ms),
        "leftover_locations": kinds,
    })
}

/// 「为什么变慢」：只发当前的指标、占用最高的几个进程名和登录项的名字。
pub fn input_for_diagnose(s: &Snapshot, login_items: &[String]) -> Value {
    let pct = |part: u64, total: u64| if total == 0 { 0 } else { (part as f64 / total as f64 * 100.0).round() as u64 };
    json!({
        "cpu_pct": s.cpu.usage_pct.round(),
        "load_1min": (s.cpu.load1 * 10.0).round() / 10.0,
        "cores": s.cpu.cores,
        "memory_used_pct": pct(s.mem.used_bytes, s.mem.total_bytes),
        "memory_pressure": s.mem.pressure,
        "disk_used_pct": pct(s.disk.used_bytes, s.disk.total_bytes),
        "top_processes": s.top.iter().take(5).map(input_for_process).collect::<Vec<_>>(),
        "login_items": login_items,
    })
}

/// 「一句话清理」：用户的原话，加上可清理的各类别。
pub fn input_for_plan(request: &str, report: &ScanReport, home: &Path) -> Value {
    let mut v = input_for_scan(report, home);
    // 原话可能很长，截到够表达意图的长度
    v["request"] = Value::String(request.chars().take(200).collect());
    v
}

/// 清理方案。类别只可能是扫描报告里已有的（规格不变量 2：AI 不能给出任意路径）。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Plan {
    pub categories: Vec<crate::model::CategoryId>,
    pub reason: String,
    pub cached: bool,
    pub backend: Backend,
}

/// 从 AI 的回答里取出方案。回答里夹着别的文字也能认；不认识的类别直接丢掉。
pub fn parse_plan(answer: &Answer, report: &ScanReport) -> Result<Plan, AiError> {
    let text = &answer.text;
    let json = text.find('{').zip(text.rfind('}')).filter(|(a, b)| a < b).map(|(a, b)| &text[a..=b]);
    let v: Value = json.and_then(|j| serde_json::from_str(j).ok()).ok_or_else(|| AiError::BadOutput(text.chars().take(200).collect()))?;
    let cleanable = |id: crate::model::CategoryId| {
        report.categories.iter().any(|c| c.id == id && !c.needs_admin && !c.unreadable && c.total_bytes > 0)
    };
    let categories = v["categories"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| serde_json::from_value(x.clone()).ok()).filter(|id| cleanable(*id)).collect())
        .unwrap_or_default();
    Ok(Plan { categories, reason: v["reason"].as_str().unwrap_or_default().to_owned(), cached: answer.cached, backend: answer.backend })
}

/// 一次最多拿这么多个下载文件去问 AI（按大小从大到小取）。
const ORGANIZE_LIMIT: usize = 60;

/// 「解释趋势」：只发每天是几天前、当天的已用量（规格切片 14）。
pub fn input_for_trend(points: &[crate::db::UsagePoint]) -> Value {
    let last = points.last().map_or(0, |p| p.day);
    json!({ "days": points.iter().map(|p| json!({ "days_ago": last - p.day, "used": human(p.used_bytes) })).collect::<Vec<_>>() })
}

/// 「整理下载」：只发文件名、大小和多久没用。
pub fn input_for_downloads(cat: &Category, now_ms: u64) -> Value {
    let files: Vec<Value> = cat
        .items
        .iter()
        .take(ORGANIZE_LIMIT)
        .enumerate()
        .map(|(n, i)| json!({ "n": n, "name": i.path.file_name().map(|f| f.to_string_lossy().into_owned()), "size": human(i.size_bytes), "idle_days": days_ago(i.last_used_ms, now_ms) }))
        .collect();
    json!({ "files": files })
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Group {
    pub title: String,
    /// delete：建议删；keep：建议留；review：请用户自己看
    pub advice: String,
    pub why: String,
    pub item_ids: Vec<String>,
    pub total_bytes: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Organized {
    pub groups: Vec<Group>,
    pub cached: bool,
    pub backend: Backend,
}

/// 从 AI 的回答里取出分组。序号越界的、重复出现的文件都丢掉；建议只认三种，其余当作「请自己看」。
pub fn parse_organized(answer: &Answer, cat: &Category) -> Result<Organized, AiError> {
    let text = &answer.text;
    let json = text.find('{').zip(text.rfind('}')).filter(|(a, b)| a < b).map(|(a, b)| &text[a..=b]);
    let v: Value = json.and_then(|j| serde_json::from_str(j).ok()).ok_or_else(|| AiError::BadOutput(text.chars().take(200).collect()))?;
    let known = cat.items.len().min(ORGANIZE_LIMIT);
    let mut seen = std::collections::HashSet::new();
    let mut groups = Vec::new();
    for g in v["groups"].as_array().into_iter().flatten().take(6) {
        let items: Vec<&Item> = g["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|n| n.as_u64().map(|n| n as usize))
            .filter(|n| *n < known && seen.insert(*n))
            .map(|n| &cat.items[n])
            .collect();
        if items.is_empty() {
            continue;
        }
        let advice = match g["advice"].as_str() {
            Some(a @ ("delete" | "keep")) => a,
            _ => "review",
        };
        groups.push(Group {
            title: g["title"].as_str().unwrap_or(t("未命名")).chars().take(20).collect(),
            advice: advice.to_owned(),
            why: g["why"].as_str().unwrap_or_default().chars().take(60).collect(),
            total_bytes: items.iter().map(|i| i.size_bytes).sum(),
            item_ids: items.iter().map(|i| i.id.clone()).collect(),
        });
    }
    Ok(Organized { groups, cached: answer.cached, backend: answer.backend })
}

/// 每种问题的提示词模板，和 `Kind` 一一对应。
fn prompt(kind: Kind, input: &Value) -> String {
    let data = serde_json::to_string_pretty(input).unwrap_or_default();
    match kind {
        // 条目和进程的解释会被缓存很久，而大小和占用一直在变，所以不让它复述数字
        Kind::ExplainItem => format!(
            "下面是 Mac 上一个可清理条目的信息（category 是它所属的清理类别）：\n{data}\n\n请在 150 字以内说明：它是什么、属于哪个应用、删除后会怎样、是否可以放心删除。不要复述大小和天数，界面上已经显示了。"
        ),
        Kind::ExplainProcess => format!(
            "下面是 Mac 上正在运行的一个进程（已按所属应用合并，cpu_pct 按单核 100% 计）：\n{data}\n\n请在 80 字以内说明：它是什么、为什么会占用这些资源、能不能退出。不要复述内存和 CPU 的具体数字，界面上已经显示了。"
        ),
        Kind::ExplainSpace => format!(
            "下面是 Mac 启动盘上一个占用空间较多的文件夹或文件（kind 为 folder 是文件夹，file 是文件）：\n{data}\n\n请在 150 字以内说明：它是什么、属于哪个应用或系统功能、能不能删、想腾空间的话该怎么正确清理（例如在哪个应用里清，而不是直接删）。不要复述大小，界面上已经显示了。"
        ),
        Kind::ExplainLoginItem => format!(
            "下面是 Mac 上一个开机自动启动的项目（kind 为 login_item 是登录时打开的应用，agent 是后台常驻的启动代理，daemon 是系统级后台服务；program 是它要运行的程序，program_missing 为 true 表示这个程序已经不在了）：\n{data}\n\n请在 100 字以内说明：它是什么、属于哪个应用、关掉它会有什么影响、建不建议保留。"
        ),
        Kind::UninstallRisk => format!(
            "用户准备卸载下面这个 Mac 应用（leftover_locations 是它在资源库里留有数据的目录类别）：\n{data}\n\n请在 100 字以内提醒：卸载会一并移除哪些本地数据、有没有需要先备份或导出的东西。没有需要注意的就直接说可以放心卸载。不要复述天数。"
        ),
        Kind::Diagnose => format!(
            "下面是一台 Mac 此刻的状态（top_processes 已按所属应用合并，cpu_pct 按单核 100% 计；login_items 是开机自动启动的项目）：\n{data}\n\n请在 150 字以内指出最可能拖慢它的两三个原因，每个原因给一句具体建议。如果各项都正常，就直接说状态良好。"
        ),
        Kind::Plan => format!(
            "用户想清理 Mac 的磁盘空间。request 是他的原话，categories 是这次扫描出来、可以清理的类别（selected_by_default 为 false 的多半是个人文件）：\n{data}\n\n请按他的要求挑出应该清理的类别。只能从 categories 里的 category 值中选，不要编造别的。只输出一个 JSON 对象，不要输出别的文字：{{\"categories\": [类别值...], \"reason\": \"80 字以内，说明选了什么、为什么没选其余的\"}}。大小已经换算好，引用时照抄。"
        ),
        Kind::OrganizeDownloads => format!(
            "下面是用户下载文件夹里很久没打开的文件（n 是序号，idle_days 是多少天没用过）：\n{data}\n\n请只根据文件名把它们分成不超过 6 组，比如安装包、重复下载、截图、文档票据、压缩包等。只输出一个 JSON 对象，不要输出别的文字：{{\"groups\": [{{\"title\": \"组名\", \"advice\": \"delete 或 keep 或 review\", \"why\": \"30 字以内的理由\", \"files\": [序号...]}}]}}。每个文件最多出现在一组里；看不出是什么的放进一个 advice 为 review 的组。"
        ),
        Kind::ExplainTrend => format!(
            "下面是一台 Mac 启动盘最近几周每天的已用空间（days_ago 是几天前）：\n{data}\n\n请在 120 字以内说明：用量在涨还是在降、大约每天变化多少，常见的可能原因（比如照片、下载、开发工具缓存、系统更新），以及一两条具体建议。大小已经换算好，引用时照抄。"
        ),
        Kind::SummarizeScan => format!(
            "下面是一次磁盘清理扫描的结果（largest 是每类里最大的几项；selected_by_default 为 true 的类别会被默认勾选）：\n{data}\n\n请在 120 字以内总结：空间主要被什么占用、默认勾选的类别清理后能腾出多少、是否会影响个人文件。大小已经换算好，引用时照抄，不要自己重新换算或相加。"
        ),
    }
}

/// 缓存键只取「问的是什么」，不取会随时间变化的数值：同一个文件夹大小变了，解释还是那个解释。
/// `variant` 是回答它的后端和模型：换了后端或模型就是另一份回答。
fn cache_key(kind: Kind, input: &Value, variant: &str) -> String {
    let identity = match kind {
        Kind::ExplainItem => json!([input["path"], input["category"]]),
        Kind::ExplainProcess => json!([input["name"]]),
        Kind::ExplainSpace => json!([input["path"]]),
        Kind::ExplainLoginItem => input.clone(),
        Kind::UninstallRisk => json!([input["bundle_id"]]),
        Kind::Diagnose | Kind::Plan | Kind::OrganizeDownloads => input.clone(),
        Kind::SummarizeScan | Kind::ExplainTrend => input.clone(),
    };
    // ponytail: 标准库的哈希不保证跨 Rust 版本稳定，升级编译器后最坏情况是缓存全部失效一次，可以接受
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (kind as u8, identity.to_string(), variant).hash(&mut h);
    format!("{:016x}", h.finish())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn cache_read(dir: &Path, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(format!("{key}.json"))).ok()?).ok()?;
    let fresh = now_ms().saturating_sub(v["ts_ms"].as_u64()?) < CACHE_TTL.as_millis() as u64;
    fresh.then(|| v["text"].as_str().map(str::to_owned)).flatten()
}

fn cache_write(dir: &Path, key: &str, text: &str) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(format!("{key}.json")), json!({ "ts_ms": now_ms(), "text": text }).to_string());
}

// ---- 后端 ----

/// Claude Code 流式输出里的一行如果带着一段新生成的回答文字，就取出来。思考过程和其它事件都不算。
fn claude_delta(line: &Value) -> Option<&str> {
    let delta = &line["event"]["delta"];
    (line["type"] == "stream_event" && delta["type"] == "text_delta").then(|| delta["text"].as_str()).flatten()
}

/// 解析 Claude Code 最后那一行结果（流式输出的 `result` 行，格式与 `--output-format json` 的输出相同）。
fn parse_claude(code: Option<i32>, stdout: &str, stderr: &str) -> Result<String, AiError> {
    let all = format!("{stdout}\n{stderr}").to_lowercase();
    let parsed: Option<Value> = serde_json::from_str(stdout.trim()).ok();
    let failed = code != Some(0) || parsed.as_ref().is_some_and(|v| v["is_error"] == true);
    if failed {
        // 未登录的识别没有实测过（见探路结论第 6 项），按常见字样判断
        if ["not logged in", "/login", "please log in", "authentication", "unauthorized"].iter().any(|k| all.contains(k)) {
            return Err(AiError::NotLoggedIn);
        }
        let msg = parsed.as_ref().and_then(|v| v["result"].as_str()).unwrap_or(stderr.trim());
        return Err(AiError::Failed(msg.chars().take(200).collect()));
    }
    match parsed.as_ref().and_then(|v| v["result"].as_str()).map(str::trim) {
        Some(text) if !text.is_empty() => Ok(text.to_owned()),
        _ => Err(AiError::BadOutput(stdout.chars().take(200).collect())),
    }
}

/// 回答一边生成一边通过 `on_delta` 送出去；返回值仍是完整的回答。
/// 超时按「多久没有任何输出」算：推理强度调高后思考会很久，只要它还在出东西就不打断。
/// `cancelled` 一旦为真就结束这个子进程（按进程号，只结束自己起的这一个）。
fn claude_cli(opts: &ClaudeOptions, system: &str, user: &str, on_delta: &(dyn Fn(&str) + Sync), cancelled: &dyn Fn() -> bool) -> Result<String, AiError> {
    let bin = claude_path().ok_or(AiError::NoBackend)?;
    // 在一个新建的空目录里运行：即使工具开关失效，它身边也没有任何文件可碰。
    // 带一个自增序号，同一毫秒里的两次提问也不会共用目录
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("pikaclean-ai-{}-{}-{}", std::process::id(), now_ms(), SEQ.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&dir).map_err(|e| AiError::Failed(e.to_string()))?;
    let result = (|| {
        let mut cmd = Command::new(&bin);
        cmd.args(["-p", "--safe-mode", "--tools", "", "--no-session-persistence", "--permission-prompts", "none"])
            .args(["--system-prompt", system, "--output-format", "stream-json", "--verbose", "--include-partial-messages"]);
        if let Some(model) = opts.model.as_deref().filter(|m| valid_model(m)) {
            cmd.args(["--model", model]);
        }
        if let Some(effort) = opts.effort {
            cmd.args(["--effort", effort.as_arg()]);
        }
        let mut child = cmd
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AiError::Failed(e.to_string()))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(user.as_bytes());
        }
        // 输出在别的线程里读，主线程只管盯超时
        let (out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
        let last_output = Mutex::new(Instant::now());
        std::thread::scope(|scope| {
            let read_out = scope.spawn(|| {
                // 只留最后的结果行；没等到它（比如一启动就失败了）就留最后一行，供报错时参考
                let (mut result, mut last) = (None, String::new());
                for line in BufReader::new(out).lines().map_while(Result::ok) {
                    *last_output.lock().unwrap() = Instant::now();
                    let v: Value = serde_json::from_str(&line).unwrap_or_default();
                    if let Some(text) = claude_delta(&v) {
                        on_delta(text);
                    }
                    if v["type"] == "result" {
                        result = Some(line);
                    } else {
                        last = line;
                    }
                }
                result.unwrap_or(last)
            });
            let read_err = scope.spawn(move || {
                let mut s = String::new();
                let _ = err.read_to_string(&mut s);
                s
            });
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) if cancelled() => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(AiError::Cancelled);
                    }
                    Ok(None) if last_output.lock().unwrap().elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(100)),
                    Ok(None) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(AiError::Timeout);
                    }
                    Err(e) => return Err(AiError::Failed(e.to_string())),
                }
            };
            parse_claude(status.code(), &read_out.join().unwrap_or_default(), &read_err.join().unwrap_or_default())
        })
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// Claude Code 认得的一个模型。`value` 为空表示用它自己的默认模型。
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct ClaudeModel {
    pub value: Option<String>,
    /// Claude Code 给的名字，带版本号，如 `Opus 5.5`；默认项是它实际指向的那个模型的名字
    pub name: String,
}

/// 从 `initialize` 控制请求的回应里取出模型清单。
fn parse_models(resp: &Value) -> Vec<ClaudeModel> {
    let list = resp["response"]["response"]["models"].as_array().map(Vec::as_slice).unwrap_or_default();
    let name = |m: &Value| m["displayName"].as_str().unwrap_or_default().to_owned();
    list.iter()
        .filter_map(|m| match m["value"].as_str()? {
            "default" => {
                let same = list.iter().find(|o| o["value"] != "default" && o["resolvedModel"] == m["resolvedModel"]);
                Some(ClaudeModel { value: None, name: same.map(name).unwrap_or_default() })
            }
            v => valid_model(v).then(|| ClaudeModel { value: Some(v.into()), name: name(m) }),
        })
        .collect()
}

/// 问本机 Claude Code 它认得哪些模型（Agent SDK 的 supportedModels 发的也是这个请求）。
/// 不提问、不花额度；拿到回应就结束进程，不等它自己退出。
pub fn claude_models() -> Result<Vec<ClaudeModel>, AiError> {
    let bin = claude_path().ok_or(AiError::NoBackend)?;
    let mut child = Command::new(bin)
        .args(["-p", "--safe-mode", "--tools", "", "--no-session-persistence", "--permission-prompts", "none"])
        .args(["--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| AiError::Failed(e.to_string()))?;
    let mut stdin = child.stdin.take().unwrap();
    let _ = writeln!(stdin, "{}", json!({ "type": "control_request", "request_id": "models", "request": { "subtype": "initialize" } }));
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let found = BufReader::new(out).lines().map_while(Result::ok).map(|l| serde_json::from_str::<Value>(&l).unwrap_or_default()).find(|v| v["type"] == "control_response");
        let _ = tx.send(found);
    });
    let got = rx.recv_timeout(Duration::from_secs(20));
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    match got {
        Ok(Some(v)) => Ok(parse_models(&v)),
        Ok(None) => Err(AiError::BadOutput(String::new())),
        Err(_) => Err(AiError::Timeout),
    }
}

/// 自填服务的地址只接受加密连接；本机地址除外（方便用跑在自己电脑上的模型）。
pub fn valid_base_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost")
}

/// 流式响应里的一行（`data: {...}`）如果带着一段新文字，就取出来。
fn sse_delta(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.strip_prefix("data:")?.trim()).ok()?;
    v["choices"][0]["delta"]["content"].as_str().filter(|t| !t.is_empty()).map(str::to_owned)
}

/// 自填服务一次回答最多等这么久。慢模型可以一直出字，但不能无限出下去；域名解析也不受空闲超时约束，靠它兜底。
const API_TOTAL: Duration = Duration::from_secs(300);

/// 调用 OpenAI 兼容的对话接口。回答一边生成一边通过 `on_delta` 送出去；返回值仍是完整的回答。
/// 和 Claude Code 一样按「多久没有任何输出」算超时（`TIMEOUT`），另有总时长上限 `API_TOTAL`。
fn api_chat(cfg: &ApiConfig, key: &str, system: &str, user: &str, on_delta: &dyn Fn(&str), cancelled: &dyn Fn() -> bool) -> Result<String, AiError> {
    api_chat_within(cfg, key, system, user, on_delta, cancelled, (TIMEOUT, API_TOTAL))
}

/// `limits` 是（空闲超时，总时长上限），测试里用短的。
fn api_chat_within(cfg: &ApiConfig, key: &str, system: &str, user: &str, on_delta: &dyn Fn(&str), cancelled: &dyn Fn() -> bool, limits: (Duration, Duration)) -> Result<String, AiError> {
    if !valid_base_url(&cfg.base_url) {
        return Err(AiError::Failed(t("服务地址必须以 https:// 开头（本机地址除外）").into()));
    }
    let (idle, total) = limits;
    let started = Instant::now();
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let body = json!({
        "model": cfg.model,
        "messages": [{ "role": "system", "content": system }, { "role": "user", "content": user }],
        "stream": true,
    });
    // 不能用 `.timeout()`：它是整个请求的总时长（含读完回答），而且设了它 `.timeout_read()` 就失效
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(idle).build();
    let resp = agent.post(&url).set("Authorization", &format!("Bearer {key}")).set("Content-Type", "application/json").send_string(&body.to_string());
    let (mut streamed, mut text) = (String::new(), String::new());
    match resp {
        Ok(r) => {
            for line in BufReader::new(r.into_reader()).lines() {
                if cancelled() {
                    return Err(AiError::Cancelled);
                }
                if started.elapsed() > total {
                    return Err(AiError::Failed(t("AI 回答超过 5 分钟，已停止").into()));
                }
                let line = line.map_err(|e| if e.to_string().contains("timed out") { AiError::Timeout } else { AiError::Failed(e.to_string()) })?;
                if let Some(chunk) = sse_delta(&line) {
                    on_delta(&chunk);
                    streamed.push_str(&chunk);
                }
                text.push_str(&line);
                text.push('\n');
            }
        }
        Err(ureq::Error::Status(401 | 403, _)) => return Err(AiError::NotLoggedIn),
        Err(ureq::Error::Status(code, r)) => {
            let detail: String = r.into_string().unwrap_or_default().chars().take(160).collect();
            return Err(AiError::Failed(t("服务返回 {0}：{1}").replace("{0}", &code.to_string()).replace("{1}", &detail)));
        }
        Err(e) if e.to_string().contains("timed out") => return Err(AiError::Timeout),
        Err(e) => return Err(AiError::Failed(e.to_string().chars().take(200).collect())),
    };
    if !streamed.trim().is_empty() {
        return Ok(streamed.trim().to_owned());
    }
    // 有的服务不支持流式，会照旧一次性返回整段 JSON
    let v: Value = serde_json::from_str(&text).map_err(|_| AiError::BadOutput(text.chars().take(200).collect()))?;
    match v["choices"][0]["message"]["content"].as_str().map(str::trim) {
        Some(t) if !t.is_empty() => Ok(t.to_owned()),
        _ => Err(AiError::BadOutput(text.chars().take(200).collect())),
    }
}

/// 向 AI 提问。相同的问题在有效期内直接返回缓存，不再调用后端。
/// 新生成的回答会一段一段地通过 `on_delta` 送出去，免得用户干等；命中缓存时不会调用它。
/// `fresh` 为真时不看缓存，重新问一次（用户点了「重新回答」），新回答照样写进缓存。
/// `ask_id` 是这次提问的编号，`cancel` 同一个编号就让它停下。
pub fn ask(kind: Kind, input: &Value, cache_dir: &Path, choice: &Choice, fresh: bool, ask_id: Option<u32>, on_delta: &(dyn Fn(&str) + Sync)) -> Result<Answer, AiError> {
    let cancelled = || ask_id.is_some_and(is_cancelled);
    let result = match choice.pick() {
        None => Err(AiError::NoBackend),
        Some(Backend::ClaudeCli) => {
            let o = &choice.claude;
            let variant = format!("claude:{}:{}", o.model.as_deref().unwrap_or(""), o.effort.map_or("", Effort::as_arg));
            ask_with(kind, input, cache_dir, Backend::ClaudeCli, &variant, fresh, &|sys, user| claude_cli(o, sys, user, on_delta, &cancelled))
        }
        Some(Backend::ApiKey) => match choice.api.as_ref() {
            None => Err(AiError::NoBackend),
            Some(cfg) => ask_with(kind, input, cache_dir, Backend::ApiKey, &format!("api:{}:{}", cfg.base_url, cfg.model), fresh, &|sys, user| {
                // 密钥只在真要调用时才从钥匙串里取，命中缓存时不读
                let key = crate::keychain::get().ok_or(AiError::NoBackend)?;
                api_chat(cfg, &key, sys, user, on_delta, &cancelled)
            }),
        },
    };
    if let Some(id) = ask_id {
        if let Some(set) = CANCELLED.lock().unwrap().as_mut() {
            set.remove(&id);
        }
    }
    result
}

fn ask_with(kind: Kind, input: &Value, cache_dir: &Path, which: Backend, variant: &str, fresh: bool, backend: &dyn Fn(&str, &str) -> Result<String, AiError>) -> Result<Answer, AiError> {
    // 换了界面语言就是另一份回答，不能拿别的语言缓存下来的来用
    let lang = crate::i18n::current();
    let key = match lang {
        crate::i18n::Lang::ZhHans => cache_key(kind, input, variant),
        other => format!("{}-{}", cache_key(kind, input, variant), other.code()),
    };
    if let Some(text) = cache_read(cache_dir, &key).filter(|_| !fresh) {
        return Ok(Answer { text, cached: true, backend: which });
    }
    // 回答语言写进系统提示词，和其余要求放在一起，不和它们打架
    let text = backend(&format!("{SYSTEM}{}", lang.ai_instruction()), &prompt(kind, input))?;
    cache_write(cache_dir, &key, &text);
    Ok(Answer { text, cached: false, backend: which })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CategoryId;
    use crate::scan::testutil::TempTree;
    use std::cell::Cell;

    fn keys(v: &Value) -> Vec<&str> {
        let mut k: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        k.sort();
        k
    }
    fn sample() -> (Category, Item) {
        let item = Item { id: "x".into(), path: "/Users/someone/Library/Caches/com.spotify.client".into(), size_bytes: 9, last_used_ms: Some(86_400_000), partial: false };
        let cat = Category {
            id: CategoryId::AppCache,
            root: "/Users/someone/Library/Caches".into(),
            total_bytes: 9,
            items: vec![item.clone()],
            default_selected: true,
            removal: crate::remove::Mode::Permanent,
            needs_admin: false,
            needs_full_disk_access: false,
            unreadable: false,
            partial: false,
            only: vec![],
        };
        (cat, item)
    }

    #[test]
    fn 发给_ai_的内容只有允许的字段且不含用户名() {
        let (cat, item) = sample();
        let home = Path::new("/Users/someone");
        let v = input_for_item(&cat, &item, home, 3 * 86_400_000);
        assert_eq!(keys(&v), ["category", "last_used_days_ago", "path", "size"]);
        assert_eq!(v["path"], "~/Library/Caches/com.spotify.client");
        assert_eq!(v["last_used_days_ago"], 2);

        let p = input_for_process(&Proc { name: "Docker".into(), pid: 42, mem_bytes: 5, cpu_pct: 1.26 });
        assert_eq!(keys(&p), ["cpu_pct", "memory", "name"]);

        let s = input_for_scan(&ScanReport { categories: vec![cat], ..Default::default() }, home);
        assert_eq!(keys(&s["categories"][0]), ["category", "largest", "selected_by_default", "total"]);
        assert_eq!(keys(&s["categories"][0]["largest"][0]), ["path", "size"]);
        assert_eq!((human(9), human(491_000_000), human(10_200_547_328), human(708_000_000_000)), ("0 KB".into(), "491 MB".into(), "10.2 GB".into(), "708 GB".into()));
        assert_eq!((human_mem(491 * 1024 * 1024), human_mem(10_200_547_328)), ("491 MB".into(), "9.5 GB".into()));
        assert!(!s.to_string().contains("someone"));
    }

    #[test]
    fn 相同的问题第二次不再调用后端() {
        let t = TempTree::new();
        let calls = Cell::new(0);
        let backend = |_: &str, _: &str| {
            calls.set(calls.get() + 1);
            Ok("这是缓存目录".to_owned())
        };
        let (cat, item) = sample();
        let home = Path::new("/Users/someone");
        let first = ask_with(Kind::ExplainItem, &input_for_item(&cat, &item, home, 0), &t.0, Backend::ClaudeCli, "", false, &backend).unwrap();
        // 大小和时间变了，问的还是同一个条目
        let bigger = Item { size_bytes: 999, last_used_ms: None, ..item };
        let second = ask_with(Kind::ExplainItem, &input_for_item(&cat, &bigger, home, 5), &t.0, Backend::ClaudeCli, "", false, &backend).unwrap();
        assert_eq!((first.cached, second.cached, calls.get()), (false, true, 1));
        assert_eq!(second.text, "这是缓存目录");
        // 换了模型是另一份回答；点「重新回答」不看缓存，新回答会覆盖旧的
        let input = input_for_item(&cat, &bigger, home, 5);
        assert!(!ask_with(Kind::ExplainItem, &input, &t.0, Backend::ClaudeCli, "claude:opus:", false, &backend).unwrap().cached);
        assert!(!ask_with(Kind::ExplainItem, &input, &t.0, Backend::ClaudeCli, "", true, &|_, _| Ok("新的回答".into())).unwrap().cached);
        let again = ask_with(Kind::ExplainItem, &input, &t.0, Backend::ClaudeCli, "", false, &backend).unwrap();
        assert_eq!((again.cached, again.text.as_str(), calls.get()), (true, "新的回答", 2));
    }

    #[test]
    fn 后端出错时原样返回且不写缓存() {
        let t = TempTree::new();
        let input = json!({ "name": "x" });
        assert_eq!(ask_with(Kind::ExplainProcess, &input, &t.0, Backend::ClaudeCli, "", false, &|_, _| Err(AiError::Timeout)), Err(AiError::Timeout));
        assert!(!ask_with(Kind::ExplainProcess, &input, &t.0, Backend::ClaudeCli, "", false, &|_, _| Ok("好".into())).unwrap().cached);
    }

    #[test]
    fn 清理方案只认扫描报告里可清理的类别() {
        let (cat, _) = sample();
        let mut admin_only = cat.clone();
        admin_only.id = CategoryId::SystemCache;
        admin_only.needs_admin = true;
        let report = ScanReport { categories: vec![cat, admin_only], ..Default::default() };
        let answer = |text: &str| Answer { text: text.into(), cached: false, backend: Backend::ClaudeCli };
        // 夹着说明文字、带代码围栏、含不存在的类别、含需要管理员的类别
        let messy = "好的，方案如下：\n```json\n{\"categories\": [\"app_cache\", \"system_cache\", \"/etc\", \"old_downloads\"], \"reason\": \"只清缓存\"}\n```";
        let plan = parse_plan(&answer(messy), &report).unwrap();
        assert_eq!((plan.categories, plan.reason.as_str()), (vec![CategoryId::AppCache], "只清缓存"));
        assert!(matches!(parse_plan(&answer("我做不到"), &report), Err(AiError::BadOutput(_))));
        assert!(parse_plan(&answer("{}"), &report).unwrap().categories.is_empty());

        let input = input_for_plan(&"删".repeat(500), &report, Path::new("/Users/someone"));
        assert_eq!(input["request"].as_str().unwrap().chars().count(), 200);
        assert!(prompt(Kind::Plan, &input).contains("只输出一个 JSON 对象"));
    }

    /// 起一个只应答一次的本机服务，返回（地址，收到的请求原文）。
    fn one_shot_server(status: &'static str, body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).unwrap_or(0);
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).into_owned();
                // 读到请求头结束，并且正文长度够了为止
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len: usize = text.to_lowercase().split("content-length: ").nth(1).and_then(|r| r.split("\r\n").next()?.trim().parse().ok()).unwrap_or(0);
                    if req.len() >= head_end + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let _ = write!(sock, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = tx.send(String::from_utf8_lossy(&req).into_owned());
        });
        (addr, rx)
    }

    #[test]
    fn 自填服务的请求格式和各种应答() {
        let cfg = |base: &str| ApiConfig { base_url: base.into(), model: "demo-model".into() };
        let (addr, got) = one_shot_server("200 OK", r#"{"choices":[{"message":{"role":"assistant","content":" 这是缓存 "}}]}"#);
        assert_eq!(api_chat(&cfg(&format!("{addr}/v1/")), "sk-secret", "系统", "用户", &|_| {}, &|| false), Ok("这是缓存".into()));
        let req = got.recv().unwrap();
        assert!(req.starts_with("POST /v1/chat/completions "), "{req}");
        assert!(req.to_lowercase().contains("authorization: bearer sk-secret"));
        let body: Value = serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["model"], "demo-model");
        assert_eq!(body["messages"], json!([{"role":"system","content":"系统"},{"role":"user","content":"用户"}]));
        assert_eq!(body["stream"], true);

        // 流式应答：一段一段送出去，最后拼成完整的回答
        let sse = "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"这是\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"缓存\"}}]}\n\ndata: [DONE]\n\n";
        let (addr, _) = one_shot_server("200 OK", sse);
        let seen = std::cell::RefCell::new(Vec::new());
        assert_eq!(api_chat(&cfg(&addr), "k", "s", "u", &|c| seen.borrow_mut().push(c.to_owned()), &|| false), Ok("这是缓存".into()));
        assert_eq!(*seen.borrow(), ["这是", "缓存"]);

        let (addr, _) = one_shot_server("401 Unauthorized", r#"{"error":"bad key"}"#);
        assert_eq!(api_chat(&cfg(&addr), "k", "s", "u", &|_| {}, &|| false), Err(AiError::NotLoggedIn));
        let (addr, _) = one_shot_server("500 Internal Server Error", "坏了");
        assert_eq!(api_chat(&cfg(&addr), "k", "s", "u", &|_| {}, &|| false), Err(AiError::Failed("服务返回 500：坏了".into())));
        let (addr, _) = one_shot_server("200 OK", r#"{"choices":[]}"#);
        assert!(matches!(api_chat(&cfg(&addr), "k", "s", "u", &|_| {}, &|| false), Err(AiError::BadOutput(_))));

        assert!(valid_base_url("https://api.example.com/v1") && valid_base_url("http://localhost:11434/v1"));
        assert!(!valid_base_url("http://api.example.com/v1") && !valid_base_url("file:///etc/passwd"));
        assert_eq!(api_chat(&cfg("http://api.example.com"), "k", "s", "u", &|_| {}, &|| false), Err(AiError::Failed("服务地址必须以 https:// 开头（本机地址除外）".into())));
    }

    /// 起一个慢慢出字的本机服务：每段之前先等若干毫秒。不带长度，写完就断开。
    fn slow_server(parts: Vec<(u64, &'static str)>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let _ = sock.read(&mut [0u8; 8192]);
            let _ = write!(sock, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n");
            for (wait, text) in parts {
                std::thread::sleep(Duration::from_millis(wait));
                if sock.write_all(text.as_bytes()).is_err() {
                    return;
                }
            }
        });
        addr
    }

    #[test]
    fn 自填服务按空闲时间超时_另有总时长上限_也能取消() {
        let cfg = |base: String| ApiConfig { base_url: base, model: "m".into() };
        let chunk = "data: {\"choices\":[{\"delta\":{\"content\":\"字\"}}]}\n\n";
        let seen = std::cell::RefCell::new(Vec::new());
        let on_delta = |c: &str| seen.borrow_mut().push(c.to_owned());

        // 出了一段之后再没动静：空闲超时，已经送出去的那段留在前端
        let addr = slow_server(vec![(0, chunk), (2000, chunk)]);
        let got = api_chat_within(&cfg(addr), "k", "s", "u", &on_delta, &|| false, (Duration::from_millis(300), Duration::from_secs(10)));
        assert_eq!((got, seen.borrow().len()), (Err(AiError::Timeout), 1));

        // 一直在出字，空闲超时不会触发；超过总时长就停
        seen.borrow_mut().clear();
        let addr = slow_server(vec![(150, chunk); 10]);
        let got = api_chat_within(&cfg(addr), "k", "s", "u", &on_delta, &|| false, (Duration::from_secs(1), Duration::from_millis(400)));
        assert_eq!(got, Err(AiError::Failed("AI 回答超过 5 分钟，已停止".into())));
        assert!((1..10).contains(&seen.borrow().len()));

        // 前端不要了：收到第一段后就停下
        seen.borrow_mut().clear();
        let addr = slow_server(vec![(100, chunk); 10]);
        let got = api_chat_within(&cfg(addr), "k", "s", "u", &on_delta, &|| !seen.borrow().is_empty(), (Duration::from_secs(1), Duration::from_secs(10)));
        assert_eq!((got, seen.borrow().len()), (Err(AiError::Cancelled), 1));
    }

    #[test]
    fn 取消按提问编号记下() {
        let id = 4_000_000_000 + std::process::id() % 1000;
        assert!(!is_cancelled(id));
        cancel(id);
        assert!(is_cancelled(id) && !is_cancelled(id + 1));
        CANCELLED.lock().unwrap().as_mut().unwrap().remove(&id);
    }

    #[test]
    fn 找_claude_时也看_nvm_和_volta_的目录() {
        let t = TempTree::new();
        for v in ["v18.0.0", "v20.1.0"] {
            std::fs::create_dir_all(t.0.join(".nvm/versions/node").join(v).join("bin")).unwrap();
        }
        let dirs = known_dirs(&t.0);
        let at = |rel: &str| dirs.iter().position(|d| *d == t.0.join(rel));
        assert!(at(".volta/bin").is_some());
        assert!(at(".nvm/versions/node/v20.1.0/bin").unwrap() < at(".nvm/versions/node/v18.0.0/bin").unwrap(), "新版本排前面");
    }

    #[test]
    fn 回答语言写在系统提示词里_不和别的要求打架() {
        let t = TempTree::new();
        let system = std::cell::RefCell::new(String::new());
        ask_with(Kind::ExplainProcess, &json!({ "name": "x" }), &t.0, Backend::ClaudeCli, "", false, &|sys, _| {
            *system.borrow_mut() = sys.to_owned();
            Ok("好".into())
        })
        .unwrap();
        assert!(!system.borrow().contains("只用简体中文"));
        assert!(system.borrow().ends_with(crate::i18n::current().ai_instruction()));
    }

    #[test]
    fn 流式输出里只取回答的文字() {
        let text = json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "缓存" } } });
        assert_eq!(claude_delta(&text), Some("缓存"));
        let thinking = json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "thinking_delta", "thinking": "嗯" } } });
        assert_eq!(claude_delta(&thinking), None);
        assert_eq!(claude_delta(&json!({ "type": "result", "result": "缓存是…" })), None);

        assert_eq!(sse_delta(r#"data: {"choices":[{"delta":{"content":"你好"}}]}"#), Some("你好".into()));
        assert_eq!(sse_delta(r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#), None);
        assert_eq!(sse_delta("data: [DONE]"), None);
        assert_eq!(sse_delta(""), None);
    }

    #[test]
    fn 模型名不能被当成别的参数() {
        assert!(["sonnet", "opus[1m]", "claude-sonnet-5-5", "us.anthropic.claude-x:0"].iter().all(|m| valid_model(m)));
        assert!(!["", "--dangerously-skip-permissions", "-p", "a b", "a;b", "模型"].iter().any(|m| valid_model(m)));
    }

    #[test]
    fn 模型清单取自命令行的回应() {
        let m = |v: &str, r: &str, n: &str| json!({ "value": v, "resolvedModel": r, "displayName": n });
        let resp = json!({ "type": "control_response", "response": { "response": { "models": [
            m("default", "claude-opus-5-5", "Default (recommended)"),
            m("opus", "claude-opus-5-5", "Opus 5.5"),
            m("claude-haiku-4-5-20251001", "claude-haiku-4-5-20251001", "Haiku 4.5"),
            m("-p", "x", "坏名字"),
        ] } } });
        let got: Vec<_> = parse_models(&resp).into_iter().map(|c| (c.value, c.name)).collect();
        assert_eq!(got, [(None, "Opus 5.5".into()), (Some("opus".into()), "Opus 5.5".into()), (Some("claude-haiku-4-5-20251001".into()), "Haiku 4.5".into())]);
        assert_eq!(parse_models(&json!({})), []);
    }

    #[test]
    fn 后端的选择() {
        let api = Some(ApiConfig { base_url: "https://x".into(), model: "m".into() });
        let with_api = Choice { preferred: None, api: api.clone(), ..Default::default() };
        assert!(with_api.available().contains(&Backend::ApiKey));
        assert_eq!(Choice { preferred: Some(Backend::ApiKey), api, ..Default::default() }.pick(), Some(Backend::ApiKey));
        // 指定的后端不可用时退回自动选择
        let none = Choice { preferred: Some(Backend::ApiKey), api: None, ..Default::default() };
        assert_eq!(none.pick(), none.available().first().copied());
    }

    #[test]
    fn 整理下载的分组只认有效的序号() {
        let (mut cat, item) = sample();
        cat.id = CategoryId::OldDownloads;
        cat.items = (0..3).map(|n| Item { id: format!("id{n}"), path: format!("/Users/someone/Downloads/文件{n}.dmg").into(), size_bytes: 100 * (n + 1), ..item.clone() }).collect();
        let input = input_for_downloads(&cat, 0);
        assert_eq!(keys(&input["files"][0]), ["idle_days", "n", "name", "size"]);
        assert_eq!(input["files"][2]["name"], "文件2.dmg");
        assert!(!input.to_string().contains("someone"), "只发文件名，不发完整路径");

        let answer = Answer {
            text: r#"分好了：{"groups":[{"title":"安装包","advice":"delete","why":"已安装","files":[0,2,2,99]},{"title":"其他","advice":"乱写的","why":"","files":[1,0]},{"title":"空组","advice":"keep","files":[]}]}"#.into(),
            cached: false,
            backend: Backend::ClaudeCli,
        };
        let out = parse_organized(&answer, &cat).unwrap();
        assert_eq!(out.groups.len(), 2, "空组不要");
        assert_eq!((out.groups[0].item_ids.clone(), out.groups[0].total_bytes, out.groups[0].advice.as_str()), (vec!["id0".to_owned(), "id2".to_owned()], 400, "delete"));
        assert_eq!((out.groups[1].item_ids.clone(), out.groups[1].advice.as_str()), (vec!["id1".to_owned()], "review"), "重复的文件只归第一组；不认识的建议当作请自己看");
        assert!(matches!(parse_organized(&Answer { text: "不会".into(), ..answer }, &cat), Err(AiError::BadOutput(_))));
    }

    #[test]
    fn 解析命令行输出() {
        assert_eq!(parse_claude(Some(0), r#"{"is_error":false,"result":" 好的 "}"#, ""), Ok("好的".into()));
        assert_eq!(parse_claude(Some(0), "不是 JSON", ""), Err(AiError::BadOutput("不是 JSON".into())));
        assert_eq!(parse_claude(Some(0), r#"{"is_error":false,"result":""}"#, ""), Err(AiError::BadOutput(r#"{"is_error":false,"result":""}"#.into())));
        assert_eq!(parse_claude(Some(1), "", "Invalid API key · Please run /login"), Err(AiError::NotLoggedIn));
        assert_eq!(parse_claude(Some(0), r#"{"is_error":true,"result":"额度用完了"}"#, ""), Err(AiError::Failed("额度用完了".into())));
    }

    #[test]
    fn 空间去向的解释只认路径() {
        let node = |size| crate::space::Node { name: "Caches".into(), path: "/Users/me/Library/Caches".into(), size_bytes: size, is_dir: true, cleanable: true, trashable: false, children: vec![], other_bytes: 0 };
        let home = Path::new("/Users/me");
        let a = input_for_space(&node(1), home);
        assert_eq!(a["path"], "~/Library/Caches");
        assert_eq!(a["kind"], "folder");
        assert_eq!(cache_key(Kind::ExplainSpace, &a, ""), cache_key(Kind::ExplainSpace, &input_for_space(&node(9_000_000_000), home), ""), "大小变了还是同一个问题");
        assert!(prompt(Kind::ExplainSpace, &a).contains("150 字"));
    }

    #[test]
    fn 解释趋势只发天数和已用量() {
        let p = |day, used_bytes| crate::db::UsagePoint { day, used_bytes, total_bytes: 9 };
        let input = input_for_trend(&[p(100, 1_000_000_000), p(103, 3_000_000_000)]);
        assert_eq!(input, json!({ "days": [{ "days_ago": 3, "used": human(1_000_000_000) }, { "days_ago": 0, "used": human(3_000_000_000) }] }));
        assert!(prompt(Kind::ExplainTrend, &input).contains("120 字"));
    }

    #[test]
    fn 启动项的解释只发名称和程序() {
        let item = crate::optimize::LoginItem {
            name: "com.example.sync".into(),
            kind: "agent",
            enabled: true,
            global: false,
            path: "/Users/me/Library/LaunchAgents/com.example.sync.plist".into(),
            program: Some("/Users/me/Applications/Sync.app/Contents/MacOS/sync".into()),
            broken: true,
            app: None,
            hidden: false,
        };
        let v = input_for_login_item(&item, Path::new("/Users/me"));
        assert_eq!(keys(&v), ["kind", "name", "program", "program_missing"]);
        assert_eq!(v["program"], "~/Applications/Sync.app/Contents/MacOS/sync");
        assert!(prompt(Kind::ExplainLoginItem, &v).contains("100 字"));
    }

    #[test]
    fn 三种问题各有自己的提示词() {
        let v = json!({ "name": "x" });
        let all = [Kind::ExplainItem, Kind::ExplainProcess, Kind::SummarizeScan, Kind::UninstallRisk].map(|k| prompt(k, &v));
        assert!(all[0].contains("150 字") && all[1].contains("80 字") && all[2].contains("120 字") && all[3].contains("100 字"));

        let app = AppInfo {
            id: "x".into(),
            name: "Demo".into(),
            bundle_id: "com.example.demo".into(),
            version: "1".into(),
            path: "/Applications/Demo.app".into(),
            size_bytes: 1,
            last_used_ms: None,
            created_ms: None,
            leftovers: vec![crate::model::Leftover { path: "/Users/someone/Library/Caches/com.example.demo".into(), size_bytes: None }],
            leftover_bytes: None,
            leftover_complete: false,
            kept: vec![],
            running: false,
            tools_running: vec![],
        };
        let input = input_for_app(&app, 0);
        assert_eq!(keys(&input), ["bundle_id", "last_used_days_ago", "leftover_locations", "name"]);
        assert_eq!(input["leftover_locations"], json!(["Caches"]));
        assert!(!input.to_string().contains("someone"));
    }
}
