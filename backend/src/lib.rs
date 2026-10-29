pub mod admin;
pub mod ai;
pub mod apps;
pub mod autostart;
pub mod care;
mod commands;
pub mod keyboard;
mod keychain;
pub mod db;
pub mod devenv;
pub mod i18n;
pub mod mascot;
pub mod metrics;
mod migrate;
pub mod model;
pub mod optimize;
pub mod permissions;
pub mod plugins;
pub mod remove;
pub mod scan;
pub mod sensors;
pub mod space;
pub mod system;
#[cfg(feature = "selftest")]
mod selftest;
mod tray;
pub mod updates;

use tauri::{ActivationPolicy, AppHandle, Emitter, Manager, RunEvent, WindowEvent};

/// 采样线程的句柄，用来在窗口刚打开时立刻叫醒它，而不是等上一轮的间隔走完。
struct SamplerThread(std::thread::Thread);

pub(crate) fn wake_sampler(app: &AppHandle) {
    if let Some(t) = app.try_state::<SamplerThread>() {
        t.0.unpark();
    }
}

/// 按设置决定程序坞图标是否出现。启动时和设置改动之后各调一次。
pub(crate) fn apply_dock(app: &AppHandle) {
    let mode = app.path().app_data_dir().map(|d| db::load_settings(&d).dock).unwrap_or_default();
    let policy = if mode == db::DockMode::Show { ActivationPolicy::Regular } else { ActivationPolicy::Accessory };
    // 自测时不进程序坞
    #[cfg(feature = "selftest")]
    let policy = if std::env::args().any(|a| a == "--selftest") { ActivationPolicy::Accessory } else { policy };
    let _ = app.set_activation_policy(policy);
}

/// 每天做一次的事：记一条启动盘已用量（算用量趋势用），磁盘快满时提醒一次。
fn daily_housekeeping(app: &AppHandle, snap: &model::Snapshot, last_alert_day: &mut Option<u64>, last_usage_day: &mut Option<u64>) {
    use tauri_plugin_notification::NotificationExt;
    let today = snap.ts_ms / 86_400_000;
    let Ok(dir) = app.path().app_data_dir() else { return };
    if *last_usage_day != Some(today) && snap.disk.total_bytes > 0 {
        // 记不下来就算了，不打扰用户；当天重启时 record_usage 自己会跳过已经记过的
        let _ = db::record_usage(&dir, today, snap.disk.used_bytes, snap.disk.total_bytes);
        *last_usage_day = Some(today);
        clean_reminder(app, &dir, snap.ts_ms);
    }
    // 先看磁盘是不是真快满了、今天提醒过没有，是才去读设置文件：绝大多数时候一次都不用读
    if !db::should_alert_low_disk(snap.disk.used_bytes, snap.disk.total_bytes, false, *last_alert_day, today) {
        return;
    }
    // ponytail: 「今天提醒过没有」只记在内存里，当天重启应用会再提醒一次；嫌烦再存进设置文件
    let muted = db::load_settings(&dir).mute_low_disk_alert;
    if db::should_alert_low_disk(snap.disk.used_bytes, snap.disk.total_bytes, muted, *last_alert_day, today) {
        let free_gb = snap.disk.total_bytes.saturating_sub(snap.disk.used_bytes) as f64 / 1_073_741_824.0;
        let _ = app.notification().builder().title(i18n::t("磁盘空间快用完了")).body(i18n::t("只剩 {0} GB 可用。打开 Pika Clean 清理一下？").replace("{0}", &format!("{free_gb:.0}"))).show();
        *last_alert_day = Some(today);
    }
}

/// 每周看一次缓存、日志、废纸篓攒了多少，超过 2 GB 发一条通知。只提醒，不删东西。
/// 扫描放到单独的线程里，不耽误采样。
fn clean_reminder(app: &AppHandle, dir: &std::path::Path, now_ms: u64) {
    let mut settings = db::load_settings(dir);
    if settings.clean_reminder_ms == 0 {
        settings.clean_reminder_ms = now_ms;
        let _ = db::save_settings(dir, &settings);
        return;
    }
    if !db::should_check_clean(now_ms, settings.clean_reminder_ms, settings.mute_clean_reminder) {
        return;
    }
    // ponytail: 读改写设置文件和界面改设置之间没加锁，一周一次，撞上的概率可以忽略
    settings.clean_reminder_ms = now_ms;
    let _ = db::save_settings(dir, &settings);
    let app = app.clone();
    std::thread::spawn(move || {
        use tauri_plugin_notification::NotificationExt;
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let report = scan::quick_scan(&home);
        let bytes: u64 = report.categories.iter().map(|c| c.total_bytes).sum();
        if bytes >= db::CLEAN_REMINDER_BYTES {
            let gb = format!("{:.1}", bytes as f64 / 1_073_741_824.0);
            let _ = app.notification().builder().title(i18n::t("可以清理一下了")).body(i18n::t("缓存、日志和废纸篓攒了 {0} GB。打开 Pika Clean 一键清理？").replace("{0}", &gb)).show();
        }
    });
}

/// 每 3 秒看一眼废纸篓：多了应用（用户把它拖了进来），就找它在资源库里留下的数据，有就打开主窗口问要不要一起清掉。
/// 没有完全磁盘访问权限时读不了废纸篓，什么都不做。
fn spawn_trash_watch(app: AppHandle) {
    std::thread::spawn(move || {
        let trash = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".Trash");
        let apps_in = || -> Option<std::collections::HashSet<std::path::PathBuf>> {
            Some(std::fs::read_dir(&trash).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "app")).collect())
        };
        // 一开始就在废纸篓里的不算
        let mut seen = apps_in();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let now = apps_in();
            let new: Vec<_> = match (&seen, &now) {
                (Some(old), Some(now)) => now.difference(old).cloned().collect(),
                _ => vec![],
            };
            seen = now;
            let muted = app.path().app_data_dir().map(|d| db::load_settings(&d).mute_trash_watch).unwrap_or(true);
            if new.is_empty() || muted {
                continue;
            }
            // ponytail: 在本应用里卸载时，应用先进废纸篓、残留随后才移走；等几秒再找，免得把正在移走的也问一遍
            std::thread::sleep(std::time::Duration::from_secs(5));
            if let Some(found) = new.iter().find_map(|p| commands::trashed_app(p)) {
                *app.state::<commands::PendingTrashed>().0.lock().unwrap() = Some(found);
                tray::show_main(&app, Some("apps"));
                let _ = app.emit_to("main", "apps:trashed", ());
            }
        }
    });
}

/// 后台采样：把每份快照存起来，并发给看得见的窗口。
fn spawn_sampler(app: AppHandle) -> std::thread::Thread {
    let handle = std::thread::spawn(move || {
        let mut sampler = metrics::Sampler::new();
        let mut last_alert_day = None;
        let mut last_usage_day = None;
        loop {
            let shown: Vec<String> =
                app.webview_windows().into_iter().filter(|(_, w)| w.is_visible().unwrap_or(false)).map(|(label, _)| label).collect();
            // 进程列表任一窗口可见就刷新；温度、风扇这些只有菜单栏面板显示，面板可见才读
            let snap = sampler.sample(!shown.is_empty(), shown.iter().any(|l| l == tray::POPOVER));
            daily_housekeeping(&app, &snap, &mut last_alert_day, &mut last_usage_day);
            tray::update_title(&app, &snap);
            *app.state::<commands::Latest>().0.lock().unwrap() = Some(snap.clone());
            // 收着的窗口不发：它重新显示时会叫醒采样线程马上补一份，曲线历史随快照下发，不会断档
            for label in &shown {
                let _ = app.emit_to(label.as_str(), "metrics:tick", &snap);
            }
            std::thread::park_timeout(if shown.is_empty() { metrics::INTERVAL_HIDDEN } else { metrics::INTERVAL_VISIBLE });
        }
    });
    handle.thread().clone()
}

fn context() -> tauri::Context {
    #[allow(unused_mut)]
    let mut ctx = tauri::generate_context!();
    // 自测用单独的数据目录：不碰用户真实的设置和清理记录，中途被关掉也不用恢复
    #[cfg(feature = "selftest")]
    if std::env::args().any(|a| a == "--selftest") {
        ctx.config_mut().identifier = "com.pikaclean.mac.selftest".into();
        // 自测的窗口放到屏幕外、不抢焦点：不打扰正在用电脑的人，也不会被顺手关掉
        for w in &mut ctx.config_mut().app.windows {
            w.focus = false;
            (w.x, w.y) = (Some(20000.0), Some(20000.0));
        }
    }
    ctx
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _, event| {
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        tray::show_main(app, None);
                    }
                })
                .build(),
        )
        .manage(commands::Latest::default())
        .manage(commands::LastScan::default())
        .manage(system::Awake::default())
        .manage(keyboard::KeyboardLock::default())
        .manage(commands::LastApps::default())
        .manage(commands::LastSpace::default())
        .manage(commands::PendingTrashed::default())
        .invoke_handler(tauri::generate_handler![
            commands::metrics_latest,
            commands::device_info,
            commands::care_report,
            commands::care_quit,
            commands::show_main,
            commands::popover_resize,
            commands::scan_start,
            commands::scan_cancel,
            commands::scan_latest,
            commands::remove_items,
            commands::clean_cancel,
            commands::trash_last,
            commands::trash_put_back,
            commands::trash_reveal,
            commands::ignored_list,
            commands::ignore_add,
            commands::ignore_remove,
            commands::full_disk_access,
            commands::ai_backends,
            commands::ai_cancel,
            commands::ai_consent,
            commands::ai_explain_item,
            commands::ai_explain_process,
            commands::ai_summarize_scan,
            commands::journal_summary,
            commands::awake_get,
            commands::awake_set,
            commands::keyboard_clean_start,
            commands::keyboard_clean_stop,
            commands::open_accessibility_settings,
            commands::volumes_list,
            commands::volume_eject,
            commands::process_quit,
            commands::mem_free,
            commands::apps_quit,
            commands::open_full_disk_access_settings,
            commands::open_sponsor_page,
            commands::admin_run,
            commands::admin_passwordless,
            commands::admin_passwordless_set,
            commands::apps_list,
            commands::apps_measure,
            commands::apps_uninstall,
            commands::apps_orphans,
            commands::apps_trash_orphans,
            commands::app_reveal,
            commands::app_open,
            commands::apps_updates,
            commands::ai_explain_uninstall,
            commands::login_items,
            commands::login_item_set,
            commands::login_item_remove,
            commands::login_items_stale,
            commands::login_item_add,
            commands::open_login_items_settings,
            commands::ai_diagnose,
            commands::settings_get,
            commands::autostart_set,
            commands::dock_mode_set,
            commands::language_set,
            commands::ai_consent_revoke,
            commands::ai_plan,
            commands::ai_api_set,
            commands::ai_api_clear,
            commands::ai_backend_set,
            commands::ai_claude_set,
            commands::ai_claude_models,
            commands::ai_organize_downloads,
            commands::onboarded_set,
            commands::low_disk_alert_set,
            commands::clean_reminder_set,
            commands::menubar_set,
            commands::hotkey_set,
            commands::trash_watch_set,
            commands::trashed_app_pending,
            commands::trashed_app_clean,
            commands::quick_clean,
            commands::plugins_list,
            commands::plugin_trash,
            commands::plugin_show,
            commands::item_reveal,
            commands::space_start,
            commands::space_cancel,
            commands::space_latest,
            commands::space_reveal,
            commands::space_trash,
            commands::ai_explain_space,
            commands::ai_explain_login_item,
            commands::devenv_report,
            commands::devenv_clean,
            commands::update_check,
            commands::update_install,
            commands::usage_trend,
            commands::week_report,
            commands::ai_explain_trend,
        ])
        .setup(|app| {
            // 最先做：后面每一步都要读设置
            migrate::run(app.handle());
            // 开机自启记的程序被挪走了（比如从下载目录挪进了「应用程序」）就改成现在的。开发版和自测不碰用户的开机自启
            if !cfg!(debug_assertions) && app.config().identifier == autostart::LABEL {
                if let (Some(home), Ok(exe)) = (std::env::var_os("HOME"), std::env::current_exe()) {
                    let _ = autostart::repair(&std::path::Path::new(&home).join("Library/LaunchAgents"), &exe);
                }
            }
            // 先定下语言再建菜单栏图标：它的右键菜单要用
            let language = app.path().app_data_dir().ok().and_then(|d| db::load_settings(&d).language);
            i18n::set(i18n::resolve(language.as_deref()));
            tray::init(app.handle())?;
            if let Ok(dir) = app.path().app_data_dir() {
                let settings = db::load_settings(&dir);
                tray::set_menubar(settings.menubar);
                // 被别的软件占着就算了，设置里再打开时会把原因告诉用户
                let _ = tray::set_hotkey(app.handle(), settings.hotkey);
            }
            // 带 --hidden 启动（开机自启）时只留菜单栏，主窗口等第一次打开时再建（配置里 create 为 false）
            if !std::env::args().any(|a| a == "--hidden") {
                tray::main_window(app.handle());
            }
            apply_dock(app.handle());
            #[cfg(target_os = "macos")]
            tray::intercept_quit(app.handle());
            app.manage(SamplerThread(spawn_sampler(app.handle().clone())));
            // 自测会卸载自己放的假应用，不要被这里打断
            #[cfg(feature = "selftest")]
            let watch = !std::env::args().any(|a| a == "--selftest");
            #[cfg(not(feature = "selftest"))]
            let watch = true;
            if watch {
                spawn_trash_watch(app.handle().clone());
            }
            #[cfg(feature = "selftest")]
            selftest::maybe_run(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // 关掉主窗口只是隐藏：菜单栏监控要继续运行。真正退出只走菜单栏图标的右键菜单（程序坞「退出」和 Cmd+Q 见 `tray::intercept_quit`）。
            WindowEvent::CloseRequested { api, .. } if window.label() == "main" => {
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Focused(false) if window.label() == tray::POPOVER => tray::on_popover_blur(window.app_handle()),
            _ => {}
        })
        .build(context())
        .expect("启动失败")
        .run(|app, event| {
            // 点程序坞图标时重新显示主窗口
            if let RunEvent::Reopen { .. } = event {
                tray::show_main(app, None);
            }
            // 自测时记下应用是怎么退出的：自测被中途关掉时靠这个分辨是谁关的
            #[cfg(feature = "selftest")]
            match &event {
                RunEvent::ExitRequested { code, .. } => eprintln!("[selftest] 收到退出请求 code={code:?}"),
                RunEvent::Exit => eprintln!("[selftest] 应用退出"),
                _ => {}
            }
        });
}
