//! 菜单栏：静态图标、下拉面板窗口。

use crate::i18n::t;
use crate::mascot;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::window::{Effect, EffectState, EffectsBuilder};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, Rect, WebviewUrl, WebviewWindowBuilder};

const TRAY_ID: &str = "main";
pub const POPOVER: &str = "popover";
pub const POPOVER_WIDTH: f64 = 520.0;
/// 面板默认高度上限（规格不变量 8）。用户展开进程解释时允许更高。
pub const POPOVER_MAX_HEIGHT: f64 = 900.0;

/// 面板上次因失去焦点而隐藏的时刻。点菜单栏图标会先让面板失焦隐藏，
/// 紧接着的点击事件不能再把它打开，否则永远关不掉。
static LAST_BLUR_HIDE: Mutex<Option<Instant>> = Mutex::new(None);

/// 主窗口；还没建过就按配置文件建一个。带 `--hidden` 开机自启时不预先建它，省下一个网页进程，第一次打开时才加载。
/// 新建的窗口从智能扫描页开始，赶不上接收紧接着发的 `nav:go`；目前只有面板的「去清理」带页面，去的正是这一页。
pub fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    if let Some(main) = app.get_webview_window("main") {
        return Some(main);
    }
    let config = app.config().app.windows.iter().find(|w| w.label == "main")?.clone();
    WebviewWindowBuilder::from_config(app, &config).and_then(|b| b.build()).ok()
}

pub fn show_main(app: &AppHandle, view: Option<&str>) {
    if let Some(pop) = app.get_webview_window(POPOVER) {
        let _ = pop.hide();
    }
    // 在程序坞点「退出」时图标被临时去掉了，打开主窗口时按设置放回来
    crate::apply_dock(app);
    if let Some(main) = main_window(app) {
        let _ = main.show();
        let _ = main.unminimize();
        let _ = main.set_focus();
    }
    crate::wake_sampler(app);
    if let Some(view) = view {
        let _ = app.emit_to("main", "nav:go", view);
    }
}

pub fn on_popover_blur(app: &AppHandle) {
    if let Some(pop) = app.get_webview_window(POPOVER) {
        let _ = pop.hide();
        *LAST_BLUR_HIDE.lock().unwrap() = Some(Instant::now());
    }
}

fn toggle_popover(app: &AppHandle, icon: Rect) {
    let Some(pop) = app.get_webview_window(POPOVER) else { return };
    if pop.is_visible().unwrap_or(false) {
        let _ = pop.hide();
        return;
    }
    if LAST_BLUR_HIDE.lock().unwrap().is_some_and(|at| at.elapsed() < Duration::from_millis(300)) {
        return;
    }
    // 面板水平居中对齐图标，贴着菜单栏下沿，不超出屏幕左右边缘
    let scale = pop.scale_factor().unwrap_or(2.0);
    let pos = icon.position.to_physical::<f64>(scale);
    let size = icon.size.to_physical::<f64>(scale);
    let width = pop.outer_size().map(|s| s.width as f64).unwrap_or(POPOVER_WIDTH * scale);
    let margin = 8.0 * scale;
    let mut x = pos.x + size.width / 2.0 - width / 2.0;
    if let Ok(Some(m)) = app.monitor_from_point(pos.x, pos.y) {
        let (left, right) = (m.position().x as f64, m.position().x as f64 + m.size().width as f64);
        x = x.min(right - width - margin).max(left + margin);
    }
    let _ = pop.set_position(PhysicalPosition::new(x, pos.y + size.height + 4.0 * scale));
    let _ = pop.show();
    let _ = pop.set_focus();
    crate::wake_sampler(app);
}

pub fn resize_popover(app: &AppHandle, height: f64) {
    if let Some(pop) = app.get_webview_window(POPOVER) {
        let _ = pop.set_size(LogicalSize::new(POPOVER_WIDTH, height.clamp(120.0, POPOVER_MAX_HEIGHT)));
    }
}

fn menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    Menu::with_items(
        app,
        &[
            &MenuItem::with_id(app, "show", t("打开主窗口"), true, None::<&str>)?,
            &MenuItem::with_id(app, "quit", t("退出 Pika Clean"), true, None::<&str>)?,
        ],
    )
}

/// 换了界面语言之后重建右键菜单。
pub fn relabel(app: &AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), menu(app)) {
        let _ = tray.set_menu(Some(menu));
    }
}

pub fn init(app: &AppHandle) -> tauri::Result<()> {
    WebviewWindowBuilder::new(app, POPOVER, WebviewUrl::App("popover.html".into()))
        .title(t("系统状态"))
        .inner_size(POPOVER_WIDTH, 660.0)
        .decorations(false)
        .transparent(true)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        .effects(EffectsBuilder::new().effect(Effect::Popover).state(EffectState::Active).radius(16.0).build())
        .build()?;

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(Image::new_owned(mascot::icon(tauri::is_dev()), mascot::SIZE, mascot::SIZE))
        .icon_as_template(true)
        .tooltip("Pika Clean")
        .menu(&menu(app)?)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app, None),
            "quit" => {
                #[cfg(feature = "selftest")]
                eprintln!("[selftest] 菜单栏图标的「退出」被点了");
                app.exit(0)
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, rect, .. } = event {
                toggle_popover(tray.app_handle(), rect);
            }
        })
        .build(app)?;
    #[cfg(target_os = "macos")]
    tray.with_inner_tray_icon(|inner| {
        let main_thread = objc2::MainThreadMarker::new().expect("菜单栏创建在主线程");
        let item = inner.ns_status_item().expect("菜单栏项已创建");
        let button = item.button(main_thread).expect("菜单栏按钮已创建");
        let image = button.image().expect("菜单栏图标已设置");
        let mut size = image.size();
        size.width = 22.0;
        size.height = 22.0;
        image.setSize(size);
        assert_eq!(image.size(), size, "菜单栏图标显示为 22 点");
    })?;
    Ok(())
}

/// 主窗口和面板都收起来，程序坞图标也去掉，只留菜单栏图标。
fn hide_to_menu_bar(app: &AppHandle) {
    for label in ["main", POPOVER] {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.hide();
        }
    }
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}

static QUIT_APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();

/// 程序坞的「退出」和 Cmd+Q 都会走系统的 `applicationShouldTerminate:`。这里改成只收到菜单栏，
/// 真正退出只走菜单栏图标右键菜单的「退出」（`app.exit` 不经过这条路）。
/// 关机、重启、注销时照常退出，不然会卡住系统。
#[cfg(target_os = "macos")]
pub fn intercept_quit(app: &AppHandle) {
    use objc2::runtime::{AnyObject, Imp, Sel};
    use objc2::{class, msg_send, sel};

    const NS_TERMINATE_CANCEL: usize = 0;
    const NS_TERMINATE_NOW: usize = 1;
    /// keyAEQuitReason：只有系统关机、重启、注销发来的退出请求才带这个属性
    const KEY_AE_QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");

    unsafe extern "C-unwind" fn should_terminate(_this: *mut AnyObject, _sel: Sel, _sender: *mut AnyObject) -> usize {
        let manager: *mut AnyObject = unsafe { msg_send![class!(NSAppleEventManager), sharedAppleEventManager] };
        let event: *mut AnyObject = unsafe { msg_send![manager, currentAppleEvent] };
        let by_system = !event.is_null() && {
            let reason: *mut AnyObject = unsafe { msg_send![event, attributeDescriptorForKeyword: KEY_AE_QUIT_REASON] };
            !reason.is_null()
        };
        match QUIT_APP.get() {
            Some(app) if !by_system => {
                hide_to_menu_bar(app);
                NS_TERMINATE_CANCEL
            }
            _ => NS_TERMINATE_NOW,
        }
    }

    let _ = QUIT_APP.set(app.clone());
    unsafe {
        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = msg_send![ns_app, delegate];
        let Some(delegate) = delegate.as_ref() else { return };
        let imp: Imp = std::mem::transmute(should_terminate as unsafe extern "C-unwind" fn(_, _, _) -> _);
        let cls = delegate.class() as *const _ as *mut _;
        let added = objc2::ffi::class_addMethod(cls, sel!(applicationShouldTerminate:), imp, c"Q@:@".as_ptr());
        debug_assert!(added.as_bool(), "应用代理已经有 applicationShouldTerminate:");
    }
}
