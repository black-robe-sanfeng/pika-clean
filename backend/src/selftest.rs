//! 自测：应用自己走一遍真实流程，并截取自己的窗口存成图片。
//!
//! 为什么需要它：开发时没有屏幕录制权限，截不了屏幕（实测连截自己的窗口也只得到全透明的图）；
//! 但网页视图可以把自己的画面交出来，这不需要权限。
//! 它验证的是真实的应用——真实的窗口、真实的后端数据、真实的 AI 调用——而不是浏览器里的样本数据。
//!
//! 用法（在 backend/ 下）：
//!   cargo build --release --features selftest,tauri/custom-protocol
//!   ./target/release/pikaclean --selftest /tmp/某个目录
//! 结束后目录里是一组截图和一份 notes.json，应用自动退出。
//! 运行大约四分钟。窗口放在屏幕外、不抢焦点、不显示菜单栏图标，所以运行时看不到它。
//! 它用单独的数据目录，不碰真实的设置和清理记录；
//! 记录边跑边写进 notes.json，中途被关掉也能看出跑到了哪一步。
//! 截图只有网页内容：窗口的毛玻璃、标题栏按钮和菜单栏图标截不到，仍需人眼确认。
//!
//! 它唯一会真实删除的东西，是它自己在 ~/Library/Caches 下临时创建的一个测试目录和一个假应用。
//! 开机启动项也只动它自己放进去的一个假启动代理和一个假登录项，结束时清干净。
//! 它不会点界面上的「清理」确认按钮。

use crate::commands::LastScan;
use crate::{db, tray};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Listener, Manager, PhysicalPosition};


const NOTE: &str = "const note=(k,v)=>window.__TAURI_INTERNALS__.invoke('plugin:event|emit',{event:'selftest:note',payload:{k,v:String(v)}});";

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// 等一次 AI 回答。设了 PIKA_SELFTEST_FAST 时只等 3 秒：重复运行时回答都在缓存里。
fn wait_ai() {
    sleep(if std::env::var_os("PIKA_SELFTEST_FAST").is_some() { 3000 } else { 30000 });
}

/// 让网页视图把自己当前的画面交出来，存成 PNG。
/// 这是应用内的接口，不经过屏幕截取，所以不需要屏幕录制权限。
/// 它只包含网页内容：窗口的毛玻璃、标题栏按钮和菜单栏图标不在其中。
fn snapshot(app: &AppHandle, label: &str, png: &Path) -> Result<String, String> {
    use objc2::runtime::AnyObject;
    let win = app.get_webview_window(label).ok_or("找不到窗口")?;
    let tiff = png.with_extension("tiff");
    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let out = tiff.clone();
    win.with_webview(move |wv| {
        let handler = block2::RcBlock::new(move |image: *mut AnyObject, _error: *mut AnyObject| {
            if image.is_null() {
                let _ = tx.send(Err("系统没有返回画面".into()));
                return;
            }
            // SAFETY: image 是系统交来的存活的 NSImage；取出的数据在本闭包内用完。
            let result = unsafe {
                let data: *mut AnyObject = objc2::msg_send![image, TIFFRepresentation];
                if data.is_null() {
                    Err("画面无法编码".to_owned())
                } else {
                    let bytes: *const u8 = objc2::msg_send![data, bytes];
                    let len: usize = objc2::msg_send![data, length];
                    std::fs::write(&out, std::slice::from_raw_parts(bytes, len)).map_err(|e| e.to_string())
                }
            };
            let _ = tx.send(result);
        });
        // SAFETY: wv.inner() 是存活的 WKWebView；本闭包运行在主线程。
        unsafe {
            let wk = wv.inner() as *mut AnyObject;
            let _: () = objc2::msg_send![wk, takeSnapshotWithConfiguration: std::ptr::null::<AnyObject>(), completionHandler: &*handler];
        }
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(8)).map_err(|_| "等画面超时".to_owned())??;
    let ok = std::process::Command::new("/usr/bin/sips").args(["-s", "format", "png"]).arg(&tiff).arg("--out").arg(png).output().is_ok_and(|o| o.status.success());
    let _ = std::fs::remove_file(&tiff);
    if !ok {
        return Err("转成 PNG 失败".into());
    }
    Ok(format!("{} 字节", std::fs::metadata(png).map(|m| m.len()).unwrap_or(0)))
}

struct Run {
    app: AppHandle,
    dir: PathBuf,
    notes: Arc<Mutex<BTreeMap<String, String>>>,
}

impl Run {
    fn note(&self, k: &str, v: impl Into<String>) {
        self.notes.lock().unwrap().insert(k.into(), v.into());
        // 边跑边存：中途被关掉也能看出跑到了哪一步
        self.save();
    }
    fn shot(&self, label: &str, name: &str) {
        let result = snapshot(&self.app, label, &self.dir.join(format!("{name}.png")));
        self.note(&format!("截图 {name}"), result.unwrap_or_else(|e| format!("失败：{e}")));
    }
    fn eval(&self, label: &str, js: &str) {
        if let Some(w) = self.app.get_webview_window(label) {
            let _ = w.eval(format!("(async()=>{{{NOTE}try{{{js}}}catch(e){{note('脚本出错',e)}}}})()"));
        }
    }
    fn save(&self) {
        let notes = self.notes.lock().unwrap();
        let _ = std::fs::write(self.dir.join("notes.json"), serde_json::to_string_pretty(&*notes).unwrap_or_default());
    }
}

fn scenario(r: &Run) {
    // 自测时不显示菜单栏图标：和正在用的那个长得一样，实测会被人当成多余的点「退出」关掉
    if let Some(t) = r.app.tray_by_id("main") {
        let _ = t.set_visible(false);
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let data_dir = r.app.path().app_data_dir().unwrap_or_default();
    // 数据目录是自测专用的（见 lib.rs 的 context），每次从全新状态开始
    if !data_dir.ends_with("com.pikaclean.mac.selftest") {
        r.note("中止", "数据目录不是自测专用的，不继续");
        return;
    }
    // AI 的回答缓存留着：带 PIKA_SELFTEST_FAST 重跑时不用再等
    for f in [db::SETTINGS, db::JOURNAL, "disabled-login-items.json"] {
        let _ = std::fs::remove_file(data_dir.join(f));
    }
    r.note("自测用的数据目录", data_dir.display().to_string());

    // 自己放一个测试目录进去，稍后只删它
    let probe = home.join(format!("Library/Caches/pika-selftest-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&probe);
    let _ = std::fs::write(probe.join("blob.bin"), vec![1u8; 2 * 1024 * 1024]);

    // 侧栏按名字找，不按位置：侧栏的项目会增减（2026-10-14 清理拆成了四项，按位置点就点错了页）
    // 固定窗口尺寸，截图才能互相比较。运行期间不要操作应用的窗口，否则截到的状态会对不上。
    if let Some(main) = r.app.get_webview_window("main") {
        let _ = main.set_size(tauri::LogicalSize::new(1000.0, 660.0));
    }
    sleep(4000);
    // 自测时窗口往往被别的窗口挡着。被挡住的网页会暂停过渡动画，滑出的面板就永远停在起点
    // （实测：面板的类名已经切换，透明度却一直是 0）。所以自测里把过渡关掉，直接看终态。
    for label in ["main", tray::POPOVER] {
        r.eval(label, "document.head.insertAdjacentHTML('beforeend','<style>*{transition:none!important;animation:none!important}</style>')");
    }
    // 第一次启动会先出现一段说明
    r.shot("main", "00-首次启动的说明");
    r.eval("main", "const d=document.querySelector('.dialog'); note('首次启动的说明', d ? d.innerText.replace(/\\n+/g,' | ') : '没有出现'); [...document.querySelectorAll('.dialog .btn')].find(b=>b.textContent.trim()==='开始使用')?.click()");
    sleep(800);
    r.shot("main", "01-扫描-未开始");
    r.eval("main", "note('扫描首页的趋势和近 7 天', [...document.querySelectorAll('.s-idle .trend')].map(x=>x.innerText.replace(/\\n+/g,' ')).join(' ‖ ') || '没有显示')");
    r.eval("main", "document.querySelector('.orb').click()");
    sleep(250);
    r.shot("main", "02-扫描-进行中");
    // 扫描本身在这台机器上要 8 秒左右（找不活跃的项目最多再占 35 秒），等够了再往下走
    for _ in 0..50 {
        sleep(1000);
        if r.app.state::<LastScan>().0.lock().unwrap().is_some() {
            break;
        }
    }
    sleep(1500);
    r.shot("main", "03-扫描-完成");
    r.eval("main", "note('扫描完成页的文字', (document.querySelector('.view:not([hidden]) .scroll')?.innerText ?? '').slice(0,600).replace(/\\n+/g,' | '))");

    r.eval("main", "[...document.querySelectorAll('.nav button')].find(b=>b.textContent.includes('智能扫描')).click()");
    sleep(900);
    r.shot("main", "04-清理空间");
    r.eval("main", "note('清理空间的类别', [...document.querySelectorAll('.view:not([hidden]) label.row')].map(x=>x.innerText.replace(/\\n+/g,' ')).join(' ‖ '))");

    r.eval("main", "document.querySelector('label.row .aib:not(:disabled)').click()");
    sleep(900);
    r.shot("main", "05-条目明细");
    r.eval("main", "const sh=document.querySelector('.sheet'); const cs=getComputedStyle(sh); const b=sh.getBoundingClientRect(); note('明细面板的实际状态', JSON.stringify({cls:sh.className, opacity:cs.opacity, translate:cs.translate, transform:cs.transform, display:cs.display, x:Math.round(b.x), y:Math.round(b.y), w:Math.round(b.width), h:Math.round(b.height), winW:innerWidth, winH:innerHeight, parent:sh.parentElement.className, parentHidden:sh.parentElement.hidden}))");
    r.eval("main", "const s=document.querySelector('.sheet .aib'); note('有没有 AI 入口', !!s); s?.click()");
    sleep(700);
    r.shot("main", "06-首次使用AI的告知");
    r.eval("main", "document.querySelector('.sheet .ai-box .btn')?.click()");
    sleep(1200);
    r.shot("main", "07-AI-等待中");
    wait_ai();
    r.shot("main", "08-AI-条目解释");
    r.eval("main", "note('AI 对条目的解释', document.querySelector('.sheet .ai-box')?.innerText.replace(/\\n+/g,' '))");

    // 让 AI 只按文件名给旧下载文件分组
    r.eval("main", "document.querySelector('.sheet .sh-hd button').click(); document.querySelector('[aria-label=\"查看旧下载文件的明细\"]')?.click()");
    sleep(900);
    r.eval("main", "document.querySelector('.sheet .pill')?.click()");
    // 这一问要给几十个文件分组，实测 30 秒不够
    wait_ai();
    wait_ai();
    r.shot("main", "08b-AI-整理下载");
    r.eval("main", "note('AI 整理下载的回答', document.querySelector('.sheet .ai-box')?.innerText.replace(/\\n+/g,' '))");
    r.eval("main", "note('AI 给旧下载分的组', [...document.querySelectorAll('.sheet .sh-b > .group')].slice(0,1).map(g=>[...g.querySelectorAll('.row')].map(x=>x.innerText.replace(/\\n+/g,' ')).join(' ‖ ')).join('') || document.querySelector('.sheet .ai-box')?.innerText)");

    // 不活跃项目的依赖目录：看明细里列了什么
    r.eval("main", "document.querySelector('.sheet .sh-hd button').click(); document.querySelector('[aria-label=\"查看不活跃项目的依赖的明细\"]')?.click()");
    sleep(900);
    r.shot("main", "08c-不活跃项目的依赖");
    r.eval("main", "note('不活跃项目的依赖', (document.querySelector('.sheet.on .sh-b')?.innerText ?? '明细没有打开').replace(/\\n+/g,' ').slice(0,600))");

    r.eval("main", "document.querySelector('.view:not([hidden])')?.scrollTo?.(0,0); document.querySelector('.view:not([hidden]) .gh')?.scrollIntoView()");
    sleep(300);

    r.eval("main", "document.querySelector('.sheet .sh-hd button').click(); [...document.querySelectorAll('.view:not([hidden]) .bar .btn')].find(b=>!b.disabled)?.click()");
    sleep(800);
    r.shot("main", "09-清理确认单");
    r.eval("main", "note('确认单的文字', document.querySelector('.dialog')?.innerText.replace(/\\n+/g,' | ')); [...document.querySelectorAll('.dialog .btn')].find(b=>b.textContent.trim()==='取消')?.click()");
    sleep(500);

    // 一句话清理：真实地问一次 AI，看它给的方案是不是只含扫描报告里的类别
    r.eval("main", "try{const p=await window.__TAURI_INTERNALS__.invoke('ai_plan',{request:'帮我腾出空间，但不要动下载的文件'}); note('一句话清理的方案', JSON.stringify(p))}catch(e){note('一句话清理的方案', '失败：'+JSON.stringify(e))}");
    wait_ai();

    // 通过真实的命令删除自己放进去的那一个测试目录
    let id = r.app.state::<LastScan>().0.lock().unwrap().as_ref().and_then(|rep| {
        rep.categories.iter().flat_map(|c| &c.items).find(|i| i.path == probe).map(|i| i.id.clone())
    });
    r.note("测试目录被扫描到了吗", if id.is_some() { "是" } else { "否" });
    if let Some(id) = id {
        let id = serde_json::to_string(&id).unwrap_or_default();
        r.eval("main", &format!("const res=await window.__TAURI_INTERNALS__.invoke('remove_items',{{ids:[{id}],dryRun:false}}); note('真实删除的返回', JSON.stringify(res))"));
        sleep(1500);
    }
    r.note("测试目录删除后还在吗", if probe.exists() { "还在（删除失败）" } else { "不在了（删除成功）" });
    let _ = std::fs::remove_dir_all(&probe);
    r.note("清理记录文件", match std::fs::read_to_string(data_dir.join(db::JOURNAL)) {
        Ok(t) => format!("共 {} 条，最后一条 {}", t.lines().count(), t.lines().last().unwrap_or("")),
        Err(e) => format!("读不到：{e}"),
    });

    r.eval("main", "[...document.querySelectorAll('.nav button')].find(b=>b.textContent.includes('智能扫描')).click(); [...document.querySelectorAll('button')].find(b=>b.textContent.includes('生成 AI 总结'))?.click()");
    wait_ai();
    r.shot("main", "10-AI-扫描总结");
    r.eval("main", "note('AI 的扫描总结', document.querySelector('.view:not([hidden]) .ai-box')?.innerText.replace(/\\n+/g,' '))");

    // ---- 卸载软件：自己放一个假应用进去，只卸它 ----
    let apps_dir = home.join("Applications");
    let made_apps_dir = !apps_dir.exists();
    let fake_app = apps_dir.join("PikaSelftest.app");
    let fake_cache = home.join("Library/Caches/com.pikaclean.selftestapp");
    let _ = std::fs::create_dir_all(fake_app.join("Contents/MacOS"));
    let _ = std::fs::write(fake_app.join("Contents/MacOS/x"), vec![2u8; 512 * 1024]);
    let _ = std::fs::write(
        fake_app.join("Contents/Info.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.pikaclean.selftestapp</string><key>CFBundleShortVersionString</key><string>9.9</string></dict></plist>"#,
    );
    let _ = std::fs::create_dir_all(&fake_cache);
    let _ = std::fs::write(fake_cache.join("blob"), vec![3u8; 256 * 1024]);

    r.eval("main", "[...document.querySelectorAll('.nav button')].find(b=>b.textContent.includes('卸载软件')).click()");
    sleep(4000);
    r.shot("main", "13-卸载软件");
    r.eval("main", "const rows=[...document.querySelectorAll('.view:not([hidden]) label.row')]; note('应用列表', rows.length+' 个；前 3 个：'+rows.slice(0,3).map(x=>x.innerText.replace(/\\n+/g,' ')).join(' ‖ '))");
    let app_id = serde_json::to_string(&fake_app.display().to_string()).unwrap_or_default();
    r.note("假应用放好了吗", if fake_app.join("Contents/Info.plist").is_file() { "是" } else { "否" });
    r.eval("main", "note('界面的应用列表里有假应用吗', [...document.querySelectorAll('.view:not([hidden]) label.row')].some(x=>x.innerText.includes('PikaSelftest')))");
    // 界面可能在假应用放好之前就读过列表了，这里让后端重新列一次再卸载
    r.eval("main", &format!("const fresh=await window.__TAURI_INTERNALS__.invoke('apps_list'); note('后端重新列出后有假应用吗', fresh.some(a=>a.name==='PikaSelftest')); const m=await window.__TAURI_INTERNALS__.invoke('apps_measure',{{id:{app_id}}}); note('假应用的残留', m.leftovers.length+' 处，'+m.leftover_bytes+' 字节，量完='+m.leftover_complete); const res=await window.__TAURI_INTERNALS__.invoke('apps_uninstall',{{ids:[{app_id}],dryRun:false}}); note('真实卸载的返回', JSON.stringify(res))"));
    sleep(5000);
    r.note("假应用卸载后", format!("应用{}，残留{}", if fake_app.exists() { "还在（失败）" } else { "不在了" }, if fake_cache.exists() { "还在（失败）" } else { "不在了" }));
    // 把自己放的东西彻底清掉：原位置和废纸篓里的都删
    for p in [fake_app.clone(), fake_cache.clone(), home.join(".Trash/PikaSelftest.app"), home.join(".Trash/com.pikaclean.selftestapp")] {
        let _ = std::fs::remove_dir_all(&p);
    }
    if made_apps_dir {
        let _ = std::fs::remove_dir(&apps_dir);
    }
    r.eval("main", "document.querySelector('.view:not([hidden]) label.row .aib')?.click()");
    sleep(900);
    r.eval("main", "document.querySelector('.view:not([hidden]) .ai-in .btn')?.click()");
    wait_ai();
    r.shot("main", "14-卸载软件-AI提醒");
    r.eval("main", "note('AI 的卸载提醒', document.querySelector('.view:not([hidden]) .ai-in .ai-box')?.innerText.replace(/\\n+/g,' '))");

    // ---- 优化速度 ----
    // 自己放一个假启动代理（只是 sleep）和一个假应用的登录项，稍后在界面里只点它们
    let fake = FakeLogin::new(&home);
    r.note("假启动项放好了吗", format!("代理：已加载={}，登录项：{}", fake.agent_loaded(), fake.login_present()));
    r.eval("main", "[...document.querySelectorAll('.nav button')].find(b=>b.textContent.includes('优化速度')).click()");
    sleep(8000);
    r.shot("main", "15-优化速度");
    r.eval("main", "note('优化页的分组', [...document.querySelectorAll('.view:not([hidden]) .gh')].map(x=>x.innerText).join(' ‖ ')); [...document.querySelectorAll('.view:not([hidden]) .btn')].find(b=>b.textContent.trim()==='运行')?.click()");
    sleep(700);
    r.shot("main", "16-管理员操作的说明");
    r.eval("main", "note('管理员操作前的说明', document.querySelector('.dialog')?.innerText.replace(/\\n+/g,' | ')); [...document.querySelectorAll('.dialog .btn')].find(b=>b.textContent.trim()==='取消')?.click(); document.querySelector('.view:not([hidden]) .pill')?.click()");
    wait_ai();
    r.shot("main", "17-AI-变慢诊断");
    r.eval("main", "note('AI 的变慢诊断', document.querySelector('.view:not([hidden]) .ai-box')?.innerText.replace(/\\n+/g,' '))");

    // 在真实界面里点开关：停用、启用、两步移除，每步之后由后端核对系统里的真实状态
    r.eval("main", "[...document.querySelectorAll('.view:not([hidden]) button.row')].find(b=>b.textContent.includes('管理开机启动项'))?.click()");
    sleep(1500);
    r.shot("main", "15b-开机启动项列表");
    r.eval("main", "note('开机启动项列表', (document.querySelector('.view:not([hidden]) .sheet.on .sh-b')?.innerText ?? '列表没有打开').replace(/\\n+/g,' ').slice(0,800))");
    for (name, label) in [(FAKE_LABEL, "代理"), (FAKE_APP, "登录项")] {
        let row = format!("const row=[...document.querySelectorAll('.view:not([hidden]) .sheet .row')].find(x=>x.innerText.includes('{name}')); if(!row) note('{label}：界面上找不到', '{name}');");
        r.eval("main", &format!("{row} row?.querySelector('.sw').click()"));
        sleep(4000);
        r.note(&format!("{label}：点停用后"), fake.state(name));
        if name == FAKE_LABEL {
            r.shot("main", "15c-停用之后");
        }
        r.eval("main", &format!("{row} row?.querySelector('.sw').click()"));
        sleep(4000);
        r.note(&format!("{label}：点启用后"), fake.state(name));
        r.eval("main", &format!("{row} const b=row?.querySelector('.btn'); b?.click(); await new Promise(r=>setTimeout(r,300)); b?.click()"));
        sleep(4000);
        r.note(&format!("{label}：点两次移除后"), fake.state(name));
    }
    r.eval("main", "note('操作后的列表', (document.querySelector('.view:not([hidden]) .sheet.on .sh-b')?.innerText ?? '').replace(/\\n+/g,' ').slice(0,800)); document.querySelector('.view:not([hidden]) .sheet .sh-hd button')?.click()");
    sleep(500);
    drop(fake);

    // ---- 设置 ----
    r.eval("main", "document.querySelector('.nav.foot button').click()");
    sleep(1500);
    r.shot("main", "18-设置");
    r.eval("main", "note('设置页', [...document.querySelectorAll('.view:not([hidden]) .row')].map(x=>x.innerText.replace(/\\n+/g,' ')).join(' ‖ '))");

    // 菜单栏面板：也放到屏幕外再截
    if let Some(pop) = r.app.get_webview_window(tray::POPOVER) {
        let _ = pop.set_position(PhysicalPosition::new(20000.0, 20000.0));
        let _ = pop.show();
        crate::wake_sampler(&r.app);
        sleep(5000);
        r.shot(tray::POPOVER, "11-菜单栏面板");
        r.eval(tray::POPOVER, "note('面板顶部的评分', document.querySelector('.hd').innerText.replace(/\\n+/g,' ') + ' ｜ ' + (document.querySelector('.hd').title||'')); note('面板的卡片', [...document.querySelectorAll('.t3 .tile')].map(x=>x.innerText.replace(/\\n+/g,' ')).join(' ‖ ')); note('面板的风扇与小工具', [...document.querySelectorAll('.pop-in > .line, .pop-in > .tile > .line')].map(x=>x.innerText.replace(/\\n+/g,' ')).join(' ‖ ')); note('面板内容高度', document.querySelector('.pop-in').offsetHeight); note('面板里的进程', [...document.querySelectorAll('.pr:not(.h)')].map(x=>x.innerText.replace(/\\n+/g,' ')).join(' ‖ ')); document.querySelector('button.pn')?.click()");
        wait_ai();
        r.shot(tray::POPOVER, "12-菜单栏面板-AI解释进程");
        r.eval(tray::POPOVER, "note('AI 对进程的解释', document.querySelector('.px .ai-box')?.innerText.replace(/\\n+/g,' '))");
        sleep(500);
        r.eval(tray::POPOVER, "note('面板的小工具按钮', [...document.querySelectorAll('.tool')].map(x=>x.innerText.trim()).join(' ‖ ')); try{const n=await window.__TAURI_INTERNALS__.invoke('keyboard_clean_start',{seconds:5}); await window.__TAURI_INTERNALS__.invoke('keyboard_clean_stop'); note('键盘清洁', '锁住了，原定 '+n+' 秒，已立刻解锁')}catch(e){note('键盘清洁', '没有锁：'+e)}");
        sleep(500);
        let _ = pop.hide();
    }

}

/// 带 `--selftest <目录>` 启动时运行自测，结束后退出应用。
pub fn maybe_run(app: &AppHandle) {
    let args: Vec<String> = std::env::args().collect();
    let Some(dir) = args.iter().position(|a| a == "--selftest").and_then(|i| args.get(i + 1)) else { return };
    let dir = PathBuf::from(dir);
    let _ = std::fs::create_dir_all(&dir);
    let notes = Arc::new(Mutex::new(BTreeMap::new()));
    let sink = notes.clone();
    app.listen_any("selftest:note", move |e| {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(e.payload()) {
            sink.lock().unwrap().insert(v["k"].as_str().unwrap_or("?").to_owned(), v["v"].as_str().unwrap_or("").to_owned());
        }
    });
    let run = Run { app: app.clone(), dir, notes };
    std::thread::spawn(move || {
        scenario(&run);
        sleep(800);
        run.save();
        run.app.exit(0);
    });
}

const FAKE_LABEL: &str = "com.pikaclean.selftest.agent";
const FAKE_APP: &str = "PikaSelftestLogin";

/// 自测自己放进去的开机启动项：一个启动代理和一个登录项。离开作用域时全部清掉（含废纸篓里的）。
struct FakeLogin {
    plist: PathBuf,
    app: PathBuf,
    made_apps_dir: bool,
    home: PathBuf,
}

fn sh(prog: &str, args: &[&str]) -> (bool, String) {
    std::process::Command::new(prog).args(args).output().map_or((false, String::new()), |o| (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned()))
}

fn gui() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

impl FakeLogin {
    fn new(home: &Path) -> Self {
        let plist = home.join(format!("Library/LaunchAgents/{FAKE_LABEL}.plist"));
        let _ = std::fs::write(&plist, format!(r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>Label</key><string>{FAKE_LABEL}</string><key>ProgramArguments</key><array><string>/bin/sleep</string><string>3600</string></array><key>RunAtLoad</key><true/></dict></plist>"#));
        sh("/bin/launchctl", &["bootstrap", &gui(), &plist.to_string_lossy()]);
        let apps_dir = home.join("Applications");
        let made_apps_dir = !apps_dir.exists();
        let app = apps_dir.join(format!("{FAKE_APP}.app"));
        let _ = std::fs::create_dir_all(app.join("Contents"));
        sh("/usr/bin/osascript", &["-e", "on run argv", "-e", "tell application \"System Events\" to make login item at end with properties {path:(item 1 of argv), hidden:false}", "-e", "end run", &app.to_string_lossy()]);
        Self { plist, app, made_apps_dir, home: home.to_owned() }
    }
    fn agent_loaded(&self) -> bool {
        sh("/bin/launchctl", &["print", &format!("{}/{FAKE_LABEL}", gui())]).0
    }
    fn agent_override(&self) -> &'static str {
        let out = sh("/bin/launchctl", &["print-disabled", &gui()]).1;
        let line = out.lines().find(|l| l.contains(&format!("\"{FAKE_LABEL}\"")));
        match line {
            Some(l) if l.contains("disabled") => "停用",
            Some(_) => "启用",
            None => "无记录",
        }
    }
    fn login_present(&self) -> bool {
        sh("/usr/bin/osascript", &["-e", "tell application \"System Events\" to get the path of every login item"]).1.contains(&*self.app.to_string_lossy())
    }
    fn state(&self, name: &str) -> String {
        if name == FAKE_LABEL {
            format!("覆盖表={}，已加载={}，配置文件在原处={}，在废纸篓={}", self.agent_override(), self.agent_loaded(), self.plist.exists(), self.home.join(format!(".Trash/{FAKE_LABEL}.plist")).exists())
        } else {
            format!("在系统登录项里={}，应用本身还在={}", self.login_present(), self.app.exists())
        }
    }
}

impl Drop for FakeLogin {
    fn drop(&mut self) {
        sh("/bin/launchctl", &["bootout", &format!("{}/{FAKE_LABEL}", gui())]);
        sh("/bin/launchctl", &["enable", &format!("{}/{FAKE_LABEL}", gui())]);
        let _ = std::fs::remove_file(&self.plist);
        let _ = std::fs::remove_file(self.home.join(format!(".Trash/{FAKE_LABEL}.plist")));
        sh("/usr/bin/osascript", &["-e", "on run argv", "-e", "tell application \"System Events\" to delete (every login item whose path is (item 1 of argv))", "-e", "end run", &self.app.to_string_lossy()]);
        let _ = std::fs::remove_dir_all(&self.app);
        if self.made_apps_dir {
            let _ = std::fs::remove_dir(self.app.parent().unwrap());
        }
    }
}
