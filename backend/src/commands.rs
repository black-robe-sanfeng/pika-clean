//! 前端能调用的全部命令。清单见 `docs/api/README.md`。

use crate::i18n::t;
use crate::ai::{self, AiError, Answer, Backend, Kind};
use crate::model::{AppInfo, CategoryId, CleanProgress, DeviceInfo, JournalSummary, RemoveResult, ScanReport, Snapshot, Trashed};
use crate::system::Awake;
use crate::{apps, autostart, db, optimize, permissions, remove, scan, system, tray};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter, Manager, State};

/// 会等子进程、读大文件的命令放到后台线程跑：不卡主线程，也不占着异步运行时的工作线程。
async fn blocking<T: Send + 'static>(job: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(job).await.map_err(|e| e.to_string())?
}

/// 用 `open` 打开系统设置的某一页，或加 `-R` 在「访达」里选中一个文件。等它交给系统处理完再返回
fn open(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    std::process::Command::new("/usr/bin/open").args(args).status().map(|_| ()).map_err(|e| e.to_string())
}

/// 最近一次采样结果，供窗口刚打开时立即取用。
#[derive(Default)]
pub struct Latest(pub Mutex<Option<Snapshot>>);

#[tauri::command]
pub fn metrics_latest(latest: State<Latest>) -> Option<Snapshot> {
    latest.0.lock().unwrap().clone()
}

#[tauri::command]
pub fn device_info() -> DeviceInfo {
    crate::metrics::device_info()
}

/// 保养页的体检：电池、硬盘寿命和耗电、写盘最多的应用。要跑系统报告，放后台线程
#[tauri::command]
pub async fn care_report() -> Result<crate::model::CareReport, String> {
    blocking(|| Ok(crate::care::report())).await
}

/// 退出保养页耗电、写盘榜上的应用
#[tauri::command]
pub async fn care_quit(name: String) -> Result<(), String> {
    blocking(move || crate::care::quit(&name)).await
}

/// 显示主窗口；`view` 只接受已知的页面名，其余忽略。
#[tauri::command]
pub fn show_main(app: AppHandle, view: Option<String>) {
    let view = view.filter(|v| matches!(v.as_str(), "scan" | "clean" | "apps" | "opt"));
    tray::show_main(&app, view.as_deref());
}

/// 菜单栏面板按内容高度调整窗口；高度在后端限幅。
#[tauri::command]
pub fn popover_resize(app: AppHandle, height: f64) {
    if height.is_finite() {
        tray::resize_popover(&app, height);
    }
}

/// 最近一次扫描报告。删除时只认这里面的条目，前端传来的只是编号。
/// 第二项是扫描时记下的每个条目的身份（设备号和 inode），删除前核对还是不是同一个文件。
#[derive(Default)]
pub struct LastScan(pub Mutex<Option<ScanReport>>, pub Mutex<remove::Stamps>);

static SCANNING: AtomicBool = AtomicBool::new(false);
static SCAN_CANCEL: AtomicBool = AtomicBool::new(false);

/// 不管扫描怎么结束（包括中途出错），都把「正在扫描」复位；出错时告诉前端扫描没了，免得界面一直转。
struct ScanGuard(AppHandle);
impl Drop for ScanGuard {
    fn drop(&mut self) {
        SCANNING.store(false, Ordering::SeqCst);
        if std::thread::panicking() {
            let _ = self.0.emit("scan:cancelled", ());
        }
    }
}

/// 开始扫描。进度走 `scan:progress` 事件，结果走 `scan:done` 事件；被取消时发 `scan:cancelled`，不留结果。已有扫描在跑时返回 false。
#[tauri::command]
pub fn scan_start(app: AppHandle) -> bool {
    if SCANNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    SCAN_CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let guard = ScanGuard(app.clone());
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let mut report = scan::scan(
            &scan::default_rules(&home),
            SystemTime::now(),
            &scan::spotlight_recent,
            &|p| {
                if !SCAN_CANCEL.load(Ordering::SeqCst) {
                    let _ = app.emit("scan:progress", &p);
                }
            },
            &SCAN_CANCEL,
        );
        // 取消后的报告不完整，不能拿来删东西，丢掉，删除仍以上一份完整的报告为准
        let cancelled = SCAN_CANCEL.load(Ordering::SeqCst);
        if !cancelled {
            crate::admin::retain_removable(&mut report);
            *app.state::<LastScan>().1.lock().unwrap() = remove::stamps(&report.categories);
            *app.state::<LastScan>().0.lock().unwrap() = Some(report.clone());
        }
        drop(guard);
        let _ = if cancelled { app.emit("scan:cancelled", ()) } else { app.emit("scan:done", &report) };
    });
    true
}

/// 停止正在进行的扫描。各类马上停手，随后发 `scan:cancelled`。没在扫描时什么都不做。
#[tauri::command]
pub fn scan_cancel() {
    if SCANNING.load(Ordering::SeqCst) {
        SCAN_CANCEL.store(true, Ordering::SeqCst);
    }
}

/// 只重扫这几类，结果并进最近一次报告，走 `scan:progress` 和 `scan:update`。已有扫描在跑时不做。
fn rescan(app: &AppHandle, ids: &'static [CategoryId]) {
    if SCANNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let guard = ScanGuard(app.clone());
        let rules: Vec<scan::Rule> = scan::default_rules(&home()).into_iter().filter(|r| ids.contains(&r.id)).collect();
        let fresh = scan::scan(&rules, SystemTime::now(), &scan::spotlight_recent, &|p| {
            let _ = app.emit("scan:progress", &p);
        }, &AtomicBool::new(false)); // 清理后的小范围重扫不响应「取消」
        let last = app.state::<LastScan>();
        last.1.lock().unwrap().extend(remove::stamps(&fresh.categories));
        let merged = {
            let mut report = last.0.lock().unwrap();
            for c in fresh.categories {
                if let Some(slot) = report.as_mut().and_then(|r| r.categories.iter_mut().find(|x| x.id == c.id)) {
                    *slot = c;
                }
            }
            report.clone()
        };
        drop(guard);
        if let Some(r) = merged {
            let _ = app.emit("scan:update", &r);
        }
    });
}

static REMOVING: AtomicBool = AtomicBool::new(false);
static CLEAN_CANCEL: AtomicBool = AtomicBool::new(false);

/// 同一时间只做一次清理；`scanning` 为 true 时扫描进行中也不做，免得拿着正在被替换的旧结果去删。离开作用域时放开。
struct Removing;

impl Removing {
    fn start(check_scanning: bool) -> Result<Self, String> {
        if check_scanning && SCANNING.load(Ordering::SeqCst) {
            return Err(t("正在扫描，请等扫描完成后再清理").into());
        }
        if REMOVING.swap(true, Ordering::SeqCst) {
            return Err(t("上一次清理还没结束").into());
        }
        CLEAN_CANCEL.store(false, Ordering::SeqCst);
        Ok(Removing)
    }
}

impl Drop for Removing {
    fn drop(&mut self) {
        REMOVING.store(false, Ordering::SeqCst);
    }
}

/// 清理进度走 `clean:progress` 事件，最多每 0.1 秒一条；开头、结尾和「等输密码」那条一定发。
fn progress_emitter(app: &AppHandle) -> impl Fn(CleanProgress) + '_ {
    let last = std::cell::Cell::new(None::<std::time::Instant>);
    move |p| {
        if p.admin || p.done == 0 || p.done == p.total || last.get().is_none_or(|at| at.elapsed() >= std::time::Duration::from_millis(100)) {
            last.set(Some(std::time::Instant::now()));
            let _ = app.emit("clean:progress", &p);
        }
    }
}

/// 让正在进行的清理在处理完手上这一项后停下。
#[tauri::command]
pub fn clean_cancel() {
    CLEAN_CANCEL.store(true, Ordering::SeqCst);
}

/// 文件已经删了，写清理记录失败不能当成清理失败：只在结果里带一句提示。
fn record(app: &AppHandle, result: &mut RemoveResult) {
    let written = app.path().app_data_dir().map_err(|e| e.to_string()).and_then(|dir| {
        db::append_journal(&dir, result).map_err(|e| e.to_string())?;
        // 只记最近一批移进废纸篓的，用来「放回原处」
        if !result.trashed.is_empty() {
            db::save_trashed(&dir, &result.trashed).map_err(|e| e.to_string())?;
        }
        Ok(())
    });
    if let Err(e) = written {
        result.warning = Some(format!("{}{e}", t("清理已完成，但写入清理记录失败：")));
    }
}

#[tauri::command]
pub fn scan_latest(last: State<LastScan>) -> Option<ScanReport> {
    last.0.lock().unwrap().clone()
}

/// 删除扫描报告里的条目。`ids` 是条目编号；不在最近一次报告里的编号会被拒绝。
/// 进度走 `clean:progress`。做完后把删掉的条目从报告里去掉并发 `scan:update`，不整份重扫；
/// 有东西进了废纸篓时，后台只重扫「废纸篓」这一类。
#[tauri::command(async)]
pub fn remove_items(app: AppHandle, last: State<LastScan>, ids: Vec<String>, dry_run: bool) -> Result<RemoveResult, String> {
    let _busy = Removing::start(true)?;
    let report = last.0.lock().unwrap().clone().ok_or(t("还没有扫描结果"))?;
    let stamps = last.1.lock().unwrap().clone();
    let mut result = remove::remove(&ids, &report, &stamps, dry_run, &progress_emitter(&app), &CLEAN_CANCEL);
    if dry_run {
        return Ok(result);
    }
    record(&app, &mut result);
    let pruned = last.0.lock().unwrap().as_mut().map(|r| {
        remove::prune(r, &ids);
        r.clone()
    });
    if let Some(r) = pruned {
        let _ = app.emit("scan:update", &r);
    }
    if result.trashed_bytes > 0 {
        rescan(&app, &[CategoryId::Trash]);
    }
    Ok(result)
}

/// 菜单栏面板的「快速清理」：重新扫一遍缓存、日志、废纸篓，跳过「不再提示」的条目，全部清掉。
/// `dry_run` 时只算能腾出多少，面板上先给用户看数字、再点一次才真删。
#[tauri::command(async)]
pub fn quick_clean(app: AppHandle, dry_run: bool) -> Result<RemoveResult, String> {
    let _busy = Removing::start(true)?;
    let ignored = app.path().app_data_dir().map(|d| db::load_settings(&d).ignored).unwrap_or_default();
    let report = scan::quick_scan(&home());
    let ids: Vec<String> =
        report.categories.iter().flat_map(|c| &c.items).filter(|i| !ignored.iter().any(|p| Path::new(p) == i.path)).map(|i| i.id.clone()).collect();
    let mut result = remove::remove(&ids, &report, &remove::stamps(&report.categories), dry_run, &|_| {}, &CLEAN_CANCEL);
    if !dry_run {
        record(&app, &mut result);
        // 主窗口里的扫描结果已经过时，这几类重扫一遍
        if app.state::<LastScan>().0.lock().unwrap().is_some() {
            rescan(&app, &[CategoryId::AppCache, CategoryId::Logs, CategoryId::Trash]);
        }
    }
    Ok(result)
}

/// 最近一次清理移到废纸篓的条目（原位置和在废纸篓里的位置）。
#[tauri::command]
pub fn trash_last(app: AppHandle) -> Vec<Trashed> {
    app.path().app_data_dir().map(|d| db::load_trashed(&d)).unwrap_or_default()
}

/// 只认最近一次清理记下的、在废纸篓里的这个位置
fn find_trashed(dir: &Path, to: &str) -> Result<Trashed, String> {
    db::load_trashed(dir).into_iter().find(|r| r.to.to_string_lossy() == to).ok_or_else(|| t("不在最近一次清理的记录里").into())
}

/// 把最近一次清理移到废纸篓的一项放回原处。`to` 必须是 `trash_last` 给出的废纸篓里的位置。
#[tauri::command(async)]
pub fn trash_put_back(app: AppHandle, to: String) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let rec = find_trashed(&dir, &to)?;
    remove::put_back(&rec, &home().join(".Trash"))?;
    let rest: Vec<Trashed> = db::load_trashed(&dir).into_iter().filter(|r| r.to != rec.to).collect();
    let _ = db::save_trashed(&dir, &rest);
    Ok(())
}

/// 在访达里显示最近一次清理移到废纸篓的一项。
#[tauri::command]
pub fn trash_reveal(app: AppHandle, to: String) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let rec = find_trashed(&dir, &to)?;
    std::process::Command::new("/usr/bin/open").arg("-R").arg(&rec.to).status().map(|_| ()).map_err(|e| e.to_string())
}

/// 「不再提示」的条目路径。
#[tauri::command]
pub fn ignored_list(app: AppHandle) -> Vec<String> {
    app.path().app_data_dir().map(|d| db::load_settings(&d).ignored).unwrap_or_default()
}

/// 对扫描结果里的一个条目「不再提示」：以后扫描照样列出，但不默认勾选。只认最近一次扫描报告里的编号。返回新的名单。
#[tauri::command]
pub fn ignore_add(app: AppHandle, last: State<LastScan>, id: String) -> Result<Vec<String>, String> {
    let path = last.0.lock().unwrap().as_ref().and_then(|r| r.categories.iter().flat_map(|c| &c.items).find(|i| i.id == id).map(|i| i.path.to_string_lossy().into_owned())).ok_or(t("条目不在扫描结果里"))?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let mut s = db::load_settings(&dir);
    if !s.ignored.contains(&path) {
        s.ignored.push(path);
    }
    db::save_settings(&dir, &s).map_err(|e| e.to_string())?;
    Ok(s.ignored)
}

/// 把一个路径移出「不再提示」名单。返回新的名单。
#[tauri::command]
pub fn ignore_remove(app: AppHandle, path: String) -> Result<Vec<String>, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let mut s = db::load_settings(&dir);
    s.ignored.retain(|p| *p != path);
    db::save_settings(&dir, &s).map_err(|e| e.to_string())?;
    Ok(s.ignored)
}

/// 在「访达」里显示扫描结果里的一个条目。只认最近一次扫描报告里的编号。
#[tauri::command]
pub async fn item_reveal(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || {
        let path = {
            let last = app.state::<LastScan>();
            let report = last.0.lock().unwrap();
            let item = report.as_ref().ok_or(t("还没有扫描结果"))?.categories.iter().flat_map(|c| &c.items).find(|i| i.id == id).ok_or(t("条目不在扫描结果里"))?;
            item.path.clone()
        };
        open(&["-R".as_ref(), path.as_os_str()])
    })
    .await
}

#[tauri::command]
pub fn full_disk_access() -> bool {
    permissions::has_full_disk_access()
}

// ---- AI ----
// 前端只传条目编号或进程名。发给 AI 的内容由后端从自己保存的数据里拼出来。

fn data_dir(app: &AppHandle) -> Result<PathBuf, AiError> {
    app.path().app_data_dir().map_err(|e| AiError::Failed(e.to_string()))
}

/// `on_delta` 是前端递进来的通道：回答一边生成一边往里送，界面就不用干等。
/// 回答要解析成结构再用的（清理方案、下载分组）不需要它，传 None。
/// 通道的编号同时是这次提问的编号，前端用它调 `ai_cancel`。`fresh` 为真时不用缓存，重新回答。
fn ask(app: &AppHandle, kind: Kind, input: serde_json::Value, on_delta: Option<&Channel<String>>, fresh: Option<bool>) -> Result<Answer, AiError> {
    let dir = data_dir(app)?;
    if !db::load_settings(&dir).ai_consent {
        return Err(AiError::ConsentRequired);
    }
    ai::ask(kind, &input, &dir.join(db::AI_CACHE_DIR), &choice(&dir), fresh.unwrap_or(false), on_delta.map(Channel::id), &|chunk| {
        if let Some(ch) = on_delta {
            let _ = ch.send(chunk.to_owned());
        }
    })
}

/// 这一次提问可以用哪些后端：从设置里读配置。钥匙串只查有没有密钥，不读内容。
fn choice(dir: &Path) -> ai::Choice {
    let s = db::load_settings(dir);
    ai::Choice { preferred: s.ai_backend, api: s.api.filter(|_| crate::keychain::exists()), claude: s.claude }
}

/// 前端不要这个回答了（界面关了或重新提问了）：让编号为 `id` 的那次提问停下，Claude Code 的子进程会被结束。
#[tauri::command]
pub fn ai_cancel(id: u32) {
    ai::cancel(id);
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 当前可用的 AI 后端；为空时前端隐藏全部 AI 入口。
#[tauri::command(async)]
pub fn ai_backends(app: AppHandle) -> Vec<Backend> {
    app.path().app_data_dir().map(|d| choice(&d).available()).unwrap_or_default()
}

/// 「一句话清理」：按用户的原话，从最近一次扫描里挑出要清理的类别。只是建议，由用户确认后才清理。
#[tauri::command(async)]
pub fn ai_plan(app: AppHandle, last: State<LastScan>, request: String, fresh: Option<bool>) -> Result<ai::Plan, AiError> {
    let report = last.0.lock().unwrap().clone().ok_or(AiError::Failed(t("还没有扫描结果").into()))?;
    let answer = ask(&app, Kind::Plan, ai::input_for_plan(&request, &report, &home()), None, fresh)?;
    ai::parse_plan(&answer, &report)
}

/// 保存自填的 AI 服务。`key` 为空表示保留已存的密钥、只改地址和模型。
#[tauri::command(async)]
pub fn ai_api_set(app: AppHandle, base_url: String, model: String, key: Option<String>) -> Result<(), String> {
    let (base_url, model) = (base_url.trim().to_owned(), model.trim().to_owned());
    if !ai::valid_base_url(&base_url) {
        return Err(t("服务地址必须以 https:// 开头（本机地址除外）").into());
    }
    if model.is_empty() {
        return Err(t("请填写模型名").into());
    }
    match key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        Some(k) => crate::keychain::set(k)?,
        None if !crate::keychain::exists() => return Err(t("请填写 API Key").into()),
        None => {}
    }
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { api: Some(ai::ApiConfig { base_url, model }), ..db::load_settings(&dir) }).map_err(|e| e.to_string())?;
    // 各窗口（含菜单栏面板）据此重新查 AI 能不能用
    let _ = app.emit("ai:changed", ());
    Ok(())
}

/// 删除自填的 AI 服务，连同钥匙串里的密钥。
#[tauri::command(async)]
pub fn ai_api_clear(app: AppHandle) -> Result<(), String> {
    crate::keychain::clear();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let s = db::load_settings(&dir);
    let ai_backend = s.ai_backend.filter(|b| *b != Backend::ApiKey);
    db::save_settings(&dir, &db::Settings { api: None, ai_backend, ..s }).map_err(|e| e.to_string())?;
    let _ = app.emit("ai:changed", ());
    Ok(())
}

/// 指定用哪个 AI 后端；传空表示自动。
#[tauri::command]
pub fn ai_backend_set(app: AppHandle, backend: Option<Backend>) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { ai_backend: backend, ..db::load_settings(&dir) }).map_err(|e| e.to_string())
}

/// 设置调用 Claude Code 时用的模型和推理强度；传空表示用 Claude Code 自己的默认值。
#[tauri::command]
pub fn ai_claude_set(app: AppHandle, model: Option<String>, effort: Option<ai::Effort>) -> Result<(), String> {
    let model = model.map(|m| m.trim().to_owned()).filter(|m| !m.is_empty());
    if model.as_deref().is_some_and(|m| !ai::valid_model(m)) {
        return Err(t("模型名只能包含字母、数字和 . _ - : [ ]").into());
    }
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { claude: ai::ClaudeOptions { model, effort }, ..db::load_settings(&dir) }).map_err(|e| e.to_string())
}

/// 本机 Claude Code 认得的模型清单，名字和版本号都由它自己给出。要启动一次命令行，耗时几秒。
#[tauri::command]
pub async fn ai_claude_models() -> Result<Vec<ai::ClaudeModel>, AiError> {
    tauri::async_runtime::spawn_blocking(ai::claude_models).await.map_err(|e| AiError::Failed(e.to_string()))?
}

/// 用户同意把条目信息发给 AI。只需同意一次。
#[tauri::command]
pub fn ai_consent(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { ai_consent: true, ..db::load_settings(&dir) }).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn ai_explain_item(app: AppHandle, last: State<LastScan>, id: String, on_delta: Channel<String>, fresh: Option<bool>) -> Result<Answer, AiError> {
    let report = last.0.lock().unwrap().clone().ok_or(AiError::Failed(t("还没有扫描结果").into()))?;
    let (cat, item) = report
        .categories
        .iter()
        .find_map(|c| c.items.iter().find(|i| i.id == id).map(|i| (c, i)))
        .ok_or(AiError::Failed(t("条目不在扫描结果里").into()))?;
    ask(&app, Kind::ExplainItem, ai::input_for_item(cat, item, &home(), now_ms()), Some(&on_delta), fresh)
}

#[tauri::command(async)]
pub fn ai_explain_process(app: AppHandle, latest: State<Latest>, name: String, on_delta: Channel<String>, fresh: Option<bool>) -> Result<Answer, AiError> {
    let snap = latest.0.lock().unwrap().clone().ok_or(AiError::Failed(t("还没有系统指标").into()))?;
    let proc = snap.top.iter().find(|p| p.name == name).ok_or(AiError::Failed(t("进程已不在列表里").into()))?;
    ask(&app, Kind::ExplainProcess, ai::input_for_process(proc), Some(&on_delta), fresh)
}

#[tauri::command(async)]
pub fn ai_summarize_scan(app: AppHandle, last: State<LastScan>, on_delta: Channel<String>, fresh: Option<bool>) -> Result<Answer, AiError> {
    let report = last.0.lock().unwrap().clone().ok_or(AiError::Failed(t("还没有扫描结果").into()))?;
    ask(&app, Kind::SummarizeScan, ai::input_for_scan(&report, &home()), Some(&on_delta), fresh)
}

// ---- 清理记录与系统小工具 ----

#[tauri::command]
pub fn journal_summary(app: AppHandle) -> JournalSummary {
    app.path().app_data_dir().map(|d| db::journal_summary(&d)).unwrap_or_default()
}

#[tauri::command]
pub fn awake_get(awake: State<Awake>) -> bool {
    awake.is_on()
}

/// 打开或关闭「常亮」（不让电脑休眠）。返回现在的状态。
#[tauri::command]
pub fn awake_set(awake: State<Awake>, on: bool) -> Result<bool, String> {
    awake.set(on)
}

#[tauri::command]
pub async fn volumes_list() -> Vec<String> {
    blocking(|| Ok(system::ejectable_volumes(Path::new("/Volumes")))).await.unwrap_or_default()
}

/// 推出一个外接磁盘。`name` 必须是 `volumes_list` 返回的名字。
#[tauri::command]
pub async fn volume_eject(name: String) -> Result<(), String> {
    blocking(move || system::eject(Path::new("/Volumes"), &name)).await
}

/// 请一个进程退出。`name` 必须在最近一次的进程列表里；进程号由后端查，前端不传。
#[tauri::command]
pub fn process_quit(app: AppHandle, latest: State<Latest>, name: String) -> Result<(), String> {
    let pid = latest.0.lock().unwrap().as_ref().and_then(|s| s.top.iter().find(|p| p.name == name).map(|p| p.pid)).ok_or(t("进程已不在列表里"))?;
    system::quit(pid)?;
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = db::append_optimize(&dir, "quit");
    }
    crate::wake_sampler(&app);
    Ok(())
}

/// 此刻的空闲内存（字节）。「释放内存」前后各量一次，算出实际腾出了多少。
#[tauri::command]
pub fn mem_free() -> Option<u64> {
    crate::metrics::free_mem_bytes()
}

/// 「释放内存」：请这几个应用退出，等它们真的退出（最多 10 秒）。返回没有退出的那些名字（比如有没保存的内容，应用在问用户）。
/// 和「退出」按钮一样，名字必须在最近一次的进程列表里，进程号由后端查。是应用的就像按 ⌘Q 那样请它退出，不是应用的发终止信号，都不强杀。
#[tauri::command]
pub async fn apps_quit(app: AppHandle, names: Vec<String>) -> Result<Vec<String>, String> {
    let found: Vec<(String, Option<u32>)> = {
        let latest = app.state::<Latest>();
        let snap = latest.0.lock().unwrap();
        names.into_iter().map(|n| {
            let pid = snap.as_ref().and_then(|s| s.top.iter().find(|p| p.name == n).map(|p| p.pid));
            (n, pid)
        }).collect()
    };
    blocking(move || {
        let mut asked = Vec::new();
        for (name, pid) in &found {
            let Some(pid) = *pid else { continue };
            if pid <= 1 || pid == std::process::id() {
                continue;
            }
            if apps::request_quit(pid) || system::quit(pid).is_ok() {
                asked.push((name.clone(), pid));
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline && asked.iter().any(|(_, p)| system::alive(*p)) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if asked.iter().any(|(_, p)| !system::alive(*p)) {
            if let Ok(dir) = app.path().app_data_dir() {
                let _ = db::append_optimize(&dir, "quit");
            }
        }
        crate::wake_sampler(&app);
        // 没找到、没请动、请了没退的，都算没退出
        Ok(found.into_iter().filter(|(n, _)| asked.iter().find(|(a, _)| a == n).is_none_or(|(_, p)| system::alive(*p))).map(|(n, _)| n).collect())
    })
    .await
}

/// 打开系统设置里授予「完全磁盘访问」的那一页。
/// 键盘清洁：锁住键盘若干秒（后端限制在 5 到 60 秒），返回实际秒数。没有「辅助功能」权限时返回 `no_access`。
#[tauri::command]
pub fn keyboard_clean_start(lock: State<crate::keyboard::KeyboardLock>, seconds: u64) -> Result<u64, String> {
    lock.start(seconds)
}

#[tauri::command]
pub fn keyboard_clean_stop(lock: State<crate::keyboard::KeyboardLock>) {
    lock.stop();
}

#[tauri::command]
pub async fn open_accessibility_settings() -> Result<(), String> {
    blocking(|| open(&["x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility".as_ref()])).await
}

#[tauri::command]
pub async fn open_full_disk_access_settings() -> Result<(), String> {
    blocking(|| open(&["x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles".as_ref()])).await
}

/// 用默认浏览器打开作者的爱发电赞助页。地址写死在这里，前端不能传任意链接进来。
#[tauri::command]
pub async fn open_sponsor_page() -> Result<(), String> {
    blocking(|| open(&["https://afdian.com/a/black-robe-sanfeng".as_ref()])).await
}

/// 免密授权的状态：开没开，以及开了之后哪些任务不再问密码。
#[derive(serde::Serialize)]
pub struct Passwordless {
    on: bool,
    tasks: Vec<crate::admin::Task>,
}

#[tauri::command]
pub fn admin_passwordless() -> Passwordless {
    Passwordless { on: crate::admin::passwordless(), tasks: crate::admin::passwordless_tasks() }
}

/// 开启或撤销免密授权。两种情况系统都会弹一次密码框。
#[tauri::command(async)]
pub fn admin_passwordless_set(on: bool) -> Result<(), crate::admin::AdminError> {
    crate::admin::set_passwordless(on)
}

/// 执行一项需要管理员权限的任务。系统会弹出密码框，授权过免密的任务除外。`task` 只能是预先定义的几个名字之一。
#[tauri::command(async)]
pub fn admin_run(app: AppHandle, task: crate::admin::Task) -> Result<(), crate::admin::AdminError> {
    crate::admin::run(task)?;
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = db::append_optimize(&dir, "admin");
    }
    crate::wake_sampler(&app);
    Ok(())
}

// ---- 卸载软件 ----

/// 最近一次列出的应用。卸载时只认这里面的应用，前端传来的只是编号。
#[derive(Default)]
pub struct LastApps(pub Mutex<Vec<AppInfo>>);

/// 列出可卸载的应用。要遍历每个应用包算大小，通常需要几秒。
#[tauri::command(async)]
pub fn apps_list(last: State<LastApps>) -> Vec<AppInfo> {
    let list = apps::list(&apps::default_roots(&home()), &apps::spotlight_index, &apps::running_checker());
    *last.0.lock().unwrap() = list.clone();
    list
}

/// 量一个应用的残留文件有多大（列表里默认不量）。最多花 15 秒，没量完时结果里会标明只是下限。
#[tauri::command(async)]
pub fn apps_measure(last: State<LastApps>, id: String) -> Result<AppInfo, String> {
    let mut info = last.0.lock().unwrap().iter().find(|a| a.id == id).cloned().ok_or(t("应用不在列表里"))?;
    if info.leftover_bytes.is_none() {
        apps::measure(&mut info, apps::LEFTOVER_BUDGET);
        if let Some(slot) = last.0.lock().unwrap().iter_mut().find(|a| a.id == id) {
            *slot = info.clone();
        }
    }
    Ok(info)
}

/// 卸载前等一个应用退出最多这么久。
const QUIT_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// 卸载应用：连同残留文件一起移到废纸篓。`ids` 必须来自最近一次 `apps_list`。
/// 正在运行的应用会先被请求正常退出；不肯退的记在失败列表里，不强行卸载。
/// 在「访达」里选中一个应用。只认当前应用列表里的编号。
#[tauri::command]
pub async fn app_reveal(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || {
        let path = app.state::<LastApps>().0.lock().unwrap().iter().find(|a| a.id == id).map(|a| a.path.clone()).ok_or(t("应用不在列表里"))?;
        open(&["-R".as_ref(), path.as_os_str()])
    })
    .await
}

/// 打开一个应用（让它自己的更新程序接手）。只认列表里有的。
#[tauri::command]
pub async fn app_open(app: AppHandle, id: String) -> Result<(), String> {
    blocking(move || {
        let path = app.state::<LastApps>().0.lock().unwrap().iter().find(|a| a.id == id).map(|a| a.path.clone()).ok_or(t("应用不在列表里"))?;
        open(&[path.as_os_str()])
    })
    .await
}

/// 查列表里用 Sparkle 自动更新的应用有没有新版本。要联网，通常几秒。
#[tauri::command(async)]
pub fn apps_updates(last: State<LastApps>) -> Vec<crate::updates::AppUpdate> {
    let apps = last.0.lock().unwrap().clone();
    crate::updates::check(&apps)
}

#[tauri::command(async)]
pub fn apps_uninstall(app: AppHandle, last: State<LastApps>, ids: Vec<String>, dry_run: bool) -> Result<RemoveResult, String> {
    let known = last.0.lock().unwrap().clone();
    let roots = apps::default_roots(&home());
    // 列表可能是几分钟前的，动手前重新查一遍哪些应用正在运行
    let running_now = apps::running_checker();
    // 主程序没开、只有包里的命令行工具在跑也算：卸载前同样要请它们退出（列表和确认单上都提示了）
    let is_running = |path: &Path| {
        let (main, tools) = running_now(path);
        main || !tools.is_empty()
    };
    let mut total = RemoveResult::default();
    let mut removed = 0;
    for (n, id) in ids.iter().enumerate() {
        let Some(info) = known.iter().find(|a| &a.id == id) else {
            total.failed.push(crate::model::Failure { path: id.into(), reason: t("不在应用列表里").into() });
            continue;
        };
        // 告诉界面正在卸哪一个、是不是在等它退出（它可能正弹着「是否保存」）
        let live = is_running(&info.path);
        let _ = app.emit("uninstall:progress", crate::model::UninstallProgress { name: info.name.clone(), done: n as u32, total: ids.len() as u32, waiting: live && !dry_run });
        // 正在运行的先请它退出并等它退完；它不肯退（比如有没保存的内容）就当作还在运行，下面会跳过它
        let running = live && (dry_run || !apps::quit_and_wait(&info.path, QUIT_WAIT));
        let info = AppInfo { running, ..info.clone() };
        let r = remove::uninstall(&info, &roots, dry_run);
        removed += r.failed.iter().all(|f| f.path != info.path) as u64;
        total.trashed_bytes += r.trashed_bytes;
        total.failed.extend(r.failed);
    }
    if !dry_run && removed > 0 {
        let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        let _ = db::append_uninstall(&dir, removed, &total);
    }
    Ok(total)
}

/// 找已删除应用留下的数据（容器、窗口状态）并量大小。只读，最多十几秒。
#[tauri::command(async)]
pub fn apps_orphans() -> Vec<crate::model::Leftover> {
    apps::orphan_leftovers(&apps::default_roots(&home()), &apps::spotlight_apps, std::time::SystemTime::now())
}

/// 把已删除应用留下的数据移到废纸篓。动手前重新找一遍，`paths` 只认仍在名单里的。
#[tauri::command(async)]
pub fn apps_trash_orphans(app: AppHandle, paths: Vec<String>) -> Result<RemoveResult, String> {
    let roots = apps::default_roots(&home());
    let current = apps::orphan_paths(&roots, &apps::spotlight_apps, std::time::SystemTime::now());
    let r = remove::trash_orphans(&paths, &current, &roots);
    if r.trashed_bytes > 0 {
        let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        let _ = db::append_uninstall(&dir, 0, &r);
    }
    Ok(r)
}

/// 刚拖进废纸篓的应用还留下的数据，等用户决定要不要一起清掉（见 `lib.rs` 的 `spawn_trash_watch`）。
#[derive(serde::Serialize, Clone, Debug)]
pub struct TrashedApp {
    pub name: String,
    /// 显示用
    pub paths: Vec<String>,
    pub bytes: u64,
    #[serde(skip)]
    real: Vec<PathBuf>,
}

#[derive(Default)]
pub struct PendingTrashed(pub Mutex<Option<TrashedApp>>);

/// 废纸篓里的应用 `app` 留下的数据，量好大小；没有就返回 None。
pub(crate) fn trashed_app(app: &Path) -> Option<TrashedApp> {
    let (name, real) = apps::trashed_leftovers(&apps::default_roots(&home()), app)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let bytes = real
        .iter()
        .map(|p| {
            let dev = p.symlink_metadata().map(|m| std::os::unix::fs::MetadataExt::dev(&m)).unwrap_or(0);
            scan::disk_usage_until(p, dev, Some(deadline)).0
        })
        .sum();
    (!real.is_empty()).then(|| TrashedApp { name, paths: real.iter().map(|p| p.to_string_lossy().into_owned()).collect(), bytes, real })
}

#[tauri::command]
pub fn trashed_app_pending(pending: State<PendingTrashed>) -> Option<TrashedApp> {
    pending.0.lock().unwrap().clone()
}

/// `clean` 为 true 时把等着的那份残留移到废纸篓，否则只是不再问。只认后端自己找出来的路径。
#[tauri::command(async)]
pub fn trashed_app_clean(app: AppHandle, pending: State<PendingTrashed>, clean: bool) -> Result<RemoveResult, String> {
    let Some(found) = pending.0.lock().unwrap().take() else { return Ok(RemoveResult::default()) };
    if !clean {
        return Ok(RemoveResult::default());
    }
    let r = remove::trash_orphans(&found.paths, &found.real, &apps::default_roots(&home()));
    if r.trashed_bytes > 0 {
        let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
        let _ = db::append_uninstall(&dir, 0, &r);
    }
    Ok(r)
}

// ---- 扩展和插件 ----

fn plugins_now() -> Vec<crate::plugins::Plugin> {
    crate::plugins::list(&home(), Path::new("/Library"))
}

#[tauri::command(async)]
pub fn plugins_list() -> Vec<crate::plugins::Plugin> {
    plugins_now()
}

/// 把用户资源库里的一个插件移到废纸篓。动手前重新列一遍，只认列表里标了能移走的。
#[tauri::command(async)]
pub fn plugin_trash(path: String) -> Result<(), String> {
    if !plugins_now().iter().any(|p| p.path == path && p.removable) {
        return Err(t("路径不在允许删除的范围内").into());
    }
    remove::trash_real(Path::new(&path))
}

/// 浏览器扩展：打开那个浏览器的扩展管理页；其余的在访达里显示。只认列表里有的。
#[tauri::command(async)]
pub fn plugin_show(path: String) -> Result<(), String> {
    let found = plugins_now().into_iter().find(|p| p.path == path).ok_or(t("路径不在允许删除的范围内"))?;
    match found.browser {
        Some(browser) => {
            let scheme = match browser {
                "Microsoft Edge" => "edge",
                "Brave Browser" => "brave",
                _ => "chrome",
            };
            let id = Path::new(&path).file_name().unwrap_or_default().to_string_lossy().into_owned();
            let url = format!("{scheme}://extensions/?id={id}");
            open(&["-a".as_ref(), browser.as_ref(), url.as_ref()])
        }
        None => open(&["-R".as_ref(), path.as_ref()]),
    }
}

/// 开关「应用拖进废纸篓时提示清理残留」。
#[tauri::command]
pub fn trash_watch_set(app: AppHandle, on: bool) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { mute_trash_watch: !on, ..db::load_settings(&dir) }).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn ai_explain_uninstall(app: AppHandle, last: State<LastApps>, id: String, on_delta: Channel<String>, fresh: Option<bool>) -> Result<Answer, AiError> {
    let known = last.0.lock().unwrap().clone();
    let info = known.iter().find(|a| a.id == id).ok_or(AiError::Failed(t("应用不在列表里").into()))?;
    ask(&app, Kind::UninstallRisk, ai::input_for_app(info, now_ms()), Some(&on_delta), fresh)
}

// ---- 优化速度 ----

fn app_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn login_items(app: AppHandle) -> Result<optimize::LoginItems, String> {
    blocking(move || Ok(optimize::login_items(&home(), &app_dir(&app)?))).await
}

/// 前端只传种类和名称；后端按种类重新列一遍，只认名单里有的。
fn find_login_item(dir: &Path, kind: &str, name: &str) -> Result<optimize::LoginItem, String> {
    optimize::list(kind, &home(), dir)?.into_iter().find(|i| i.name == name).ok_or_else(|| t("这一项已经不在开机启动列表里").into())
}

#[tauri::command]
pub async fn login_item_set(app: AppHandle, kind: String, name: String, on: bool) -> Result<(), String> {
    blocking(move || {
        let dir = app_dir(&app)?;
        optimize::set_enabled(&find_login_item(&dir, &kind, &name)?, on, &dir)
    })
    .await
}

/// 一键清理时顺带停用的启动项（规格 O-04 的「建议关闭」）：程序已经不存在、却还开着的启动代理。
/// 只读配置文件，不去问「系统事件」，所以不会在清理页弹出自动化授权。
#[tauri::command]
pub async fn login_items_stale(app: AppHandle) -> Result<Vec<optimize::LoginItem>, String> {
    blocking(move || {
        let dir = app_dir(&app)?;
        Ok(optimize::list("agent", &home(), &dir)?.into_iter().filter(|i| i.broken && i.enabled).collect())
    })
    .await
}

#[tauri::command]
pub async fn login_item_remove(app: AppHandle, kind: String, name: String) -> Result<(), String> {
    blocking(move || {
        let dir = app_dir(&app)?;
        optimize::remove(&find_login_item(&dir, &kind, &name)?, &dir)
    })
    .await
}

/// 让用户选一个应用加进登录时打开的列表。返回 false 表示用户取消了。
#[tauri::command]
pub async fn login_item_add() -> Result<bool, String> {
    blocking(optimize::add_chosen).await
}

/// 打开系统设置里管理登录项的那一页。
#[tauri::command]
pub async fn open_login_items_settings() -> Result<(), String> {
    blocking(|| open(&["x-apple.systempreferences:com.apple.LoginItems-Settings.extension".as_ref()])).await
}

/// 让 AI 解释一个开机启动项。前端只传种类和名称，后端在名单里找到后只发名称和程序路径。
#[tauri::command]
pub async fn ai_explain_login_item(app: AppHandle, kind: String, name: String, on_delta: Channel<String>, fresh: Option<bool>) -> Result<Answer, AiError> {
    tauri::async_runtime::spawn_blocking(move || {
        let item = find_login_item(&data_dir(&app)?, &kind, &name).map_err(AiError::Failed)?;
        ask(&app, Kind::ExplainLoginItem, ai::input_for_login_item(&item, &home()), Some(&on_delta), fresh)
    })
    .await
    .map_err(|e| AiError::Failed(e.to_string()))?
}

#[tauri::command(async)]
pub fn ai_diagnose(app: AppHandle, latest: State<Latest>, on_delta: Channel<String>, fresh: Option<bool>) -> Result<Answer, AiError> {
    let snap = latest.0.lock().unwrap().clone().ok_or(AiError::Failed(t("还没有系统指标").into()))?;
    // 和优化页上列出的是同一份名单（只算启用着的），免得 AI 说的数目和界面对不上
    let names: Vec<String> = optimize::login_items(&home(), &data_dir(&app)?).items.into_iter().filter(|i| i.enabled).map(|i| i.name).collect();
    ask(&app, Kind::Diagnose, ai::input_for_diagnose(&snap, &names), Some(&on_delta), fresh)
}

// ---- 设置 ----

#[derive(serde::Serialize)]
pub struct SettingsView {
    pub autostart: bool,
    pub ai_consent: bool,
    pub dock: db::DockMode,
    pub ai_backends: Vec<Backend>,
    /// 用户指定的后端；空表示自动
    pub ai_backend: Option<Backend>,
    /// 调用 Claude Code 时用的模型和推理强度
    pub claude: ai::ClaudeOptions,
    /// 自填服务的地址和模型。密钥本身从不返回给界面，只说有没有
    pub api: Option<ai::ApiConfig>,
    pub api_key_set: bool,
    pub onboarded: bool,
    pub low_disk_alert: bool,
    pub clean_reminder: bool,
    pub hotkey: bool,
    pub trash_watch: bool,
    pub menubar: db::MenubarShow,
    pub full_disk_access: bool,
    pub version: String,
    /// 用户选的界面语言；空表示跟随系统
    pub language: Option<String>,
    /// 实际在用的界面语言（跟随系统时是按系统语言定下来的那个）
    pub lang: &'static str,
}

fn agents_dir() -> PathBuf {
    home().join("Library/LaunchAgents")
}

#[tauri::command(async)]
pub fn settings_get(app: AppHandle) -> SettingsView {
    let dir = app.path().app_data_dir().unwrap_or_default();
    let stored = db::load_settings(&dir);
    SettingsView {
        autostart: autostart::is_enabled(&agents_dir()),
        ai_consent: stored.ai_consent,
        dock: stored.dock,
        ai_backends: choice(&dir).available(),
        ai_backend: stored.ai_backend,
        claude: stored.claude,
        api: stored.api,
        api_key_set: crate::keychain::exists(),
        onboarded: stored.onboarded,
        low_disk_alert: !stored.mute_low_disk_alert,
        clean_reminder: !stored.mute_clean_reminder,
        hotkey: stored.hotkey,
        trash_watch: !stored.mute_trash_watch,
        menubar: stored.menubar,
        full_disk_access: permissions::has_full_disk_access(),
        version: app.package_info().version.to_string(),
        language: stored.language,
        lang: crate::i18n::current().code(),
    }
}

/// 打开或关闭开机自启。
#[tauri::command]
pub fn autostart_set(on: bool) -> Result<bool, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if on && autostart::is_temporary(&exe) {
        return Err(t("请先把 Pika Clean 拖进「应用程序」文件夹，从那里打开后再开启开机自启").into());
    }
    autostart::set(&agents_dir(), &exe, on).map_err(|e| e.to_string())?;
    Ok(on)
}

/// 切换界面语言；传空表示跟随系统。后端的文字立即换，各个窗口收到 `lang:changed` 后自己重新加载。
#[tauri::command]
pub fn language_set(app: AppHandle, language: Option<String>) -> Result<(), String> {
    if language.as_deref().is_some_and(|l| crate::i18n::Lang::parse(l).is_none()) {
        return Err(t("不支持这种语言").into());
    }
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { language: language.clone(), ..db::load_settings(&dir) }).map_err(|e| e.to_string())?;
    let lang = crate::i18n::resolve(language.as_deref());
    crate::i18n::set(lang);
    crate::tray::relabel(&app);
    let _ = app.emit("lang:changed", lang.code());
    Ok(())
}

/// 设置程序坞图标的显示方式，立即生效。
#[tauri::command]
pub fn dock_mode_set(app: AppHandle, mode: db::DockMode) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { dock: mode, ..db::load_settings(&dir) }).map_err(|e| e.to_string())?;
    crate::apply_dock(&app);
    // 隐藏程序坞图标时系统会让应用退到后台，把正在操作的设置窗口拉回前面
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.set_focus();
    }
    Ok(())
}

/// 撤回「可以把条目信息发给 AI」的同意。下次用 AI 时会重新询问。
#[tauri::command]
pub fn ai_consent_revoke(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { ai_consent: false, ..db::load_settings(&dir) }).map_err(|e| e.to_string())
}

// ---- 整理下载、首次引导 ----

/// 让 AI 只根据文件名给「旧下载文件」分组。只是建议，不会勾选或删除任何东西。
#[tauri::command(async)]
pub fn ai_organize_downloads(app: AppHandle, last: State<LastScan>, fresh: Option<bool>) -> Result<ai::Organized, AiError> {
    let report = last.0.lock().unwrap().clone().ok_or(AiError::Failed(t("还没有扫描结果").into()))?;
    let cat = report.categories.iter().find(|c| c.id == crate::model::CategoryId::OldDownloads && !c.items.is_empty()).ok_or(AiError::Failed(t("没有旧下载文件").into()))?;
    let answer = ask(&app, Kind::OrganizeDownloads, ai::input_for_downloads(cat, now_ms()), None, fresh)?;
    ai::parse_organized(&answer, cat)
}

/// 记下用户已经看过首次启动的说明。
#[tauri::command]
pub fn onboarded_set(app: AppHandle) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { onboarded: true, ..db::load_settings(&dir) }).map_err(|e| e.to_string())
}

/// 开关全局快捷键 ⌃⌥P。先注册成功再存设置：被别的软件占用时报错，设置不变。
#[tauri::command]
pub fn hotkey_set(app: AppHandle, on: bool) -> Result<(), String> {
    crate::tray::set_hotkey(&app, on).map_err(|e| crate::i18n::t("快捷键被别的软件占用了：{0}").replace("{0}", &e))?;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { hotkey: on, ..db::load_settings(&dir) }).map_err(|e| e.to_string())
}

/// 开关「每周提醒清理」。
#[tauri::command]
pub fn clean_reminder_set(app: AppHandle, on: bool) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { mute_clean_reminder: !on, ..db::load_settings(&dir) }).map_err(|e| e.to_string())
}

/// 开关「磁盘快满时提醒」。
#[tauri::command]
pub fn low_disk_alert_set(app: AppHandle, on: bool) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { mute_low_disk_alert: !on, ..db::load_settings(&dir) }).map_err(|e| e.to_string())
}

/// 菜单栏图标旁边显示哪几项。存下来并马上生效。
#[tauri::command]
pub fn menubar_set(app: AppHandle, menubar: db::MenubarShow) -> Result<(), String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    db::save_settings(&dir, &db::Settings { menubar, ..db::load_settings(&dir) }).map_err(|e| e.to_string())?;
    crate::tray::set_menubar(menubar);
    crate::wake_sampler(&app);
    Ok(())
}

// ---- 空间去向 ----

/// 最近一次空间统计。「在访达中显示」和「AI 解释」只认这里面的路径。
#[derive(Default)]
pub struct LastSpace(pub Mutex<Option<crate::space::SpaceReport>>);

static SPACE_RUNNING: AtomicBool = AtomicBool::new(false);
static SPACE_CANCEL: AtomicBool = AtomicBool::new(false);

/// 开始统计启动盘的空间去向。进度走 `space:progress`，结果走 `space:done`。已在统计时返回 false。
#[tauri::command]
pub fn space_start(app: AppHandle) -> bool {
    if SPACE_RUNNING.swap(true, Ordering::SeqCst) {
        return false;
    }
    SPACE_CANCEL.store(false, Ordering::SeqCst);
    std::thread::spawn(move || {
        let roots = crate::space::cleanable_roots(&home());
        let mut report = crate::space::scan(Path::new(crate::space::DATA_ROOT), &home(), &roots, crate::space::LIMITS, &SPACE_CANCEL, &|p| {
            let _ = app.emit("space:progress", &p);
        });
        report.purgeable_bytes = crate::metrics::purgeable();
        crate::space::mark_trashable(&mut report.root, &home());
        let state = app.state::<LastSpace>();
        let report = {
            let mut last = state.0.lock().unwrap();
            match last.as_ref() {
                Some(prev) if crate::space::keep_previous(&report, Some(prev)) => prev.clone(),
                _ => {
                    *last = Some(report.clone());
                    save_space(&app, &report);
                    report
                }
            }
        };
        SPACE_RUNNING.store(false, Ordering::SeqCst);
        let _ = app.emit("space:done", &report);
    });
    true
}

/// 停止正在进行的统计。已有一份数完的结果时，`space:done` 发回的还是它（不拿没数完的覆盖）；
/// 没有时发回已数到的部分，`finished` 为 false。
#[tauri::command]
pub fn space_cancel() {
    SPACE_CANCEL.store(true, Ordering::SeqCst);
}

/// 最近一次的统计结果。应用刚启动时从磁盘读上次存下的。
#[tauri::command]
pub async fn space_latest(app: AppHandle) -> Option<crate::space::SpaceReport> {
    blocking(move || {
        let state = app.state::<LastSpace>();
        let mut last = state.0.lock().unwrap();
        if last.is_none() {
            *last = app.path().app_data_dir().ok().and_then(|dir| db::load_space(&dir));
        }
        let report = last.clone();
        Ok(report)
    })
    .await
    .unwrap_or_default()
}

/// 存下统计结果，失败只影响下次打开要不要重新数，不打断用户
fn save_space(app: &AppHandle, report: &crate::space::SpaceReport) {
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = db::save_space(&dir, report);
    }
}

/// 在访达里显示空间统计里的一项。路径必须是最近一次统计结果里的节点。
#[tauri::command]
pub async fn space_reveal(app: AppHandle, path: String) -> Result<(), String> {
    blocking(move || {
        let path = app.state::<LastSpace>().0.lock().unwrap().as_ref().and_then(|r| crate::space::find(&r.root, &path)).map(|n| n.path.clone()).ok_or(t("不在空间统计结果里"))?;
        open(&["-R".as_ref(), path.as_ref()])
    })
    .await
}

/// 把空间统计里的一项移到废纸篓，返回改好的统计结果（不重新统计，这一项的大小记到废纸篓下）。只认个人文件夹里的个人文件（规格切片 18）。
#[tauri::command]
pub async fn space_trash(app: AppHandle, path: String) -> Result<crate::space::SpaceReport, String> {
    blocking(move || trash_space_node(&app, &path)).await
}

fn trash_space_node(app: &AppHandle, path: &str) -> Result<crate::space::SpaceReport, String> {
    let state = app.state::<LastSpace>();
    let mut last = state.0.lock().unwrap();
    let report = last.as_mut().ok_or(t("不在空间统计结果里"))?;
    // 显示用路径去掉的是数据卷前缀，个人文件夹从 / 走和从数据卷走是同一个位置；从 / 走，废纸篓里「放回原处」的路径才是用户认得的
    let result = remove::trash_space(path, report, Path::new("/"), &remove::trash_real);
    if let Some(f) = result.failed.first() {
        return Err(f.reason.clone());
    }
    crate::space::move_to_trash(&mut report.root, path, &format!("{}/.Trash", home().display()));
    save_space(app, report);
    // 已经移进废纸篓了：记录写不进去只少算一次累计，不能让界面以为没移走
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = db::append_journal(&dir, &result);
    }
    Ok(report.clone())
}

#[tauri::command(async)]
pub fn ai_explain_space(app: AppHandle, last: State<LastSpace>, path: String, on_delta: Channel<String>, fresh: Option<bool>) -> Result<Answer, AiError> {
    let node = last.0.lock().unwrap().as_ref().and_then(|r| crate::space::find(&r.root, &path).cloned()).ok_or(AiError::Failed(t("不在空间统计结果里").into()))?;
    ask(&app, Kind::ExplainSpace, ai::input_for_space(&node, &home()), Some(&on_delta), fresh)
}

// ---- 开发环境 ----

#[tauri::command(async)]
pub fn devenv_report() -> crate::devenv::DevEnvReport {
    crate::devenv::report(&home())
}

/// 用 Docker 和 Xcode 自己的命令清理。删除前重新列一次，编号和「有没有设备在用」以此刻为准。
/// 进度走 `clean:progress`（第几条命令）；`clean_cancel` 后不再开始下一条。不看扫描结果，所以扫描中也能做。
#[tauri::command(async)]
pub fn devenv_clean(app: AppHandle, targets: Vec<remove::Target>, dry_run: bool) -> Result<RemoveResult, String> {
    let _busy = Removing::start(false)?;
    let before = crate::devenv::report(&home());
    let docker = ai::locate("docker");
    let mut result = remove::clean_devenv(&targets, &before, docker.as_deref(), dry_run, &crate::devenv::run, &progress_emitter(&app), &CLEAN_CANCEL);
    let after = if dry_run { before.clone() } else { crate::devenv::report(&home()) };
    (result.freed_bytes, result.estimated) = crate::devenv::freed(&before, &after, &targets);
    if !dry_run {
        record(&app, &mut result);
    }
    Ok(result)
}


// ---- 自动更新 ----

#[derive(serde::Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: Option<String>,
}

/// 去 `tauri.conf.json` 里配的地址查有没有新版本，没有返回空。
#[tauri::command]
pub async fn update_check(app: AppHandle) -> Result<Option<UpdateInfo>, String> {
    use tauri_plugin_updater::UpdaterExt;
    // ponytail: 开发版不自更新，否则会拿正式包覆盖 target/debug 里的程序
    if cfg!(debug_assertions) {
        return Ok(None);
    }
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    Ok(update.map(|u| UpdateInfo { version: u.version, notes: u.body }))
}

/// 下载并装好新版本，然后重启。安装包的签名由插件用配置里的公钥校验，对不上就不装。
/// 下载进度（0~100）往 `on_progress` 里送，服务器没给总大小时不送。重查不到新版本（已被撤回）时返回成功、不重启。
#[tauri::command]
pub async fn update_install(app: AppHandle, on_progress: Channel<u8>) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    // ponytail: 重新查一次而不是把弹框时查到的结果存起来，多一次请求换来不用管状态
    let update = app.updater().map_err(|e| e.to_string())?.check().await.map_err(|e| e.to_string())?;
    let Some(update) = update else { return Ok(()) };
    let (mut got, mut sent) = (0u64, None);
    update
        .download_and_install(
            |chunk, total| {
                got += chunk as u64;
                let Some(total) = total.filter(|&t| t > 0) else { return };
                let pct = (got.min(total) * 100 / total) as u8;
                if sent != Some(pct) {
                    sent = Some(pct);
                    let _ = on_progress.send(pct);
                }
            },
            || {},
        )
        .await
        .map_err(|e| e.to_string())?;
    app.restart()
}

// ---- 用量趋势 ----

/// 最近 28 天的启动盘用量和照这个速度几天后写满。记录不足两天时为空。
#[tauri::command]
pub fn usage_trend(app: AppHandle) -> Option<db::UsageTrend> {
    db::usage_trend(&db::load_usage(&app.path().app_data_dir().ok()?))
}

/// 扫描首页的「近 7 天」：清理记录加启动盘用量变化，全在本机算。
#[tauri::command]
pub fn week_report(app: AppHandle) -> Option<db::WeekReport> {
    let dir = app.path().app_data_dir().ok()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as u64;
    Some(db::WeekReport { journal: db::journal_summary_since(&dir, now.saturating_sub(db::WEEK_DAYS * 86_400_000)), disk_delta_bytes: db::week_delta(&db::load_usage(&dir)) })
}

/// 让 AI 解释用量趋势：只发每天是几天前和当天的已用量。
#[tauri::command(async)]
pub fn ai_explain_trend(app: AppHandle, on_delta: Channel<String>, fresh: Option<bool>) -> Result<Answer, AiError> {
    let trend = db::usage_trend(&db::load_usage(&data_dir(&app)?)).ok_or(AiError::Failed(t("用量记录还不够两天").into()))?;
    ask(&app, Kind::ExplainTrend, ai::input_for_trend(&trend.points), Some(&on_delta), fresh)
}
