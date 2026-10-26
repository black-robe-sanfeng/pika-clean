//! 键盘清洁：短时间内吞掉所有按键，方便擦键盘。鼠标不受影响。
//!
//! 不会把人锁在外面，靠三条：最长 60 秒必定自动恢复；随时可以用鼠标点「解锁」；
//! 应用退出或崩溃时，系统会自己收回拦截。
//! 拦截按键需要用户在系统设置里给本应用「辅助功能」权限，没有时直接报错，不做任何事。

use crate::i18n::t;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub const MIN_SECONDS: u64 = 5;
pub const MAX_SECONDS: u64 = 60;
/// 没有「辅助功能」权限时的错误标记，前端靠它提示用户去授权。
pub const NO_ACCESS: &str = "no_access";
/// 按下、抬起、修饰键变化、媒体键（音量、亮度等）
const KEY_EVENTS: u64 = (1 << 10) | (1 << 11) | (1 << 12) | (1 << 14);

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, events: u64, callback: extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void, user_info: *mut c_void) -> *mut c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopCommonModes: *const c_void;
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFMachPortCreateRunLoopSource(allocator: *const c_void, port: *mut c_void, order: isize) -> *mut c_void;
    fn CFMachPortInvalidate(port: *mut c_void);
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
    fn CFRelease(cf: *const c_void);
}

/// 被拦截的事件一律丢弃。系统因为超时等原因停用拦截时会发来两种特殊通知，那时原样放行。
/// ponytail: 被系统停用后不去重新启用——那样键盘只是提前恢复正常，方向是安全的
extern "C" fn swallow(_proxy: *mut c_void, kind: u32, event: *mut c_void, _info: *mut c_void) -> *mut c_void {
    if kind >= 0xFFFF_FFFE {
        event
    } else {
        std::ptr::null_mut()
    }
}

/// 在当前线程上拦截 `events`，直到超过 `max` 或 `active` 被置为 false。
/// 拦截建立成功与否通过 `started` 告诉调用方；建立不了（没有权限）时立刻返回。
fn intercept(events: u64, max: Duration, active: &AtomicBool, started: impl FnOnce(Result<(), String>)) {
    // SAFETY: 这几个都是系统的 C 接口，参数按文档传；创建出来的两个对象都在本函数结束前释放。
    unsafe {
        // 1 = 当前登录会话；0 = 插在最前面；0 = 可以改动事件（而不是只旁听）
        let tap = CGEventTapCreate(1, 0, 0, events, swallow, std::ptr::null_mut());
        if tap.is_null() {
            active.store(false, Ordering::SeqCst);
            return started(Err(NO_ACCESS.into()));
        }
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
        started(Ok(()));
        let deadline = Instant::now() + max;
        while active.load(Ordering::SeqCst) && Instant::now() < deadline {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.2, 0);
        }
        CFMachPortInvalidate(tap);
        CFRelease(source);
        CFRelease(tap);
        active.store(false, Ordering::SeqCst);
    }
}

/// 键盘锁的状态。同一时间只会有一把锁。
#[derive(Default)]
pub struct KeyboardLock(Arc<AtomicBool>);

impl KeyboardLock {
    /// 锁住键盘。返回实际会锁多少秒（请求值被限制在 5 到 60 之间）。
    pub fn start(&self, seconds: u64) -> Result<u64, String> {
        let seconds = seconds.clamp(MIN_SECONDS, MAX_SECONDS);
        if self.0.swap(true, Ordering::SeqCst) {
            return Err(t("键盘已经锁着了").into());
        }
        let (tx, rx) = mpsc::channel();
        let active = self.0.clone();
        std::thread::spawn(move || intercept(KEY_EVENTS, Duration::from_secs(seconds), &active, |r| drop(tx.send(r))));
        rx.recv_timeout(Duration::from_secs(3)).map_err(|_| t("没能锁住键盘").to_owned())?.map(|()| seconds)
    }

    /// 提前解锁。没锁着时什么也不做。
    pub fn stop(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拦截到时间自己结束_没有权限时报错而不是卡住() {
        // 测试里不碰真的按键：只拦一种这台电脑上不会出现的事件（数位板靠近），所以有没有权限都不影响正在用电脑的人
        let active = AtomicBool::new(true);
        let (tx, rx) = mpsc::channel();
        let began = Instant::now();
        intercept(1 << 25, Duration::from_millis(300), &active, |r| drop(tx.send(r)));
        let started = rx.recv().unwrap();
        assert!(started == Ok(()) || started == Err(NO_ACCESS.to_owned()), "{started:?}");
        assert!(!active.load(Ordering::SeqCst) && began.elapsed() < Duration::from_secs(2));
        println!("这台机器上的结果：{started:?}，用时 {:?}", began.elapsed());
    }

    #[test]
    fn 已经锁着时不能再锁_解锁后状态复位() {
        let lock = KeyboardLock(Arc::new(AtomicBool::new(true)));
        assert!(lock.start(30).is_err());
        lock.stop();
        assert!(!lock.0.load(Ordering::SeqCst));
        assert_eq!((3u64.clamp(MIN_SECONDS, MAX_SECONDS), 600u64.clamp(MIN_SECONDS, MAX_SECONDS)), (5, 60));
    }
}
