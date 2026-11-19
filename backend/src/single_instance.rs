//! 单实例保护：同一用户只允许一个 pikaclean 在跑。
//!
//! 为什么需要它：开机「开机自启」写的是 `~/Library/LaunchAgents/com.pikaclean.mac.plist`，
//! 而系统设置的「登录项」里还登记着这个应用本身（BTM 里的 app 类型条目）。两者互不相干，
//! 登录时会各拉起一份，两份各建一个菜单栏图标。实测两条 spawn 在同一毫秒发生：
//!
//! ```text
//! 15:38:24.495 launchd [gui/501/com.pikaclean.mac [7632]:] Successfully spawned pikaclean[7632] because speculative
//! 15:38:24.495 launchd [gui/501/application.com.pikaclean.mac.xxx [7655]:] Successfully spawned pikaclean[7655] because launch job demand
//! ```
//!
//! 用 `flock` 而不是「先连 socket、连不上就 bind」那套（`tauri-plugin-single-instance` 的 macOS 实现）：
//! 那种做法在 `connect` 失败和 `bind` 之间有窗口期，两个实例同时启动会双双认定自己是唯一实例，
//! 后 bind 的遇到 `File exists` 只打一行日志就照常启动。`flock` 是内核级原子操作，没有这个窗口。

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

/// 锁文件路径。按用户和运行模式分开：自测（`--selftest`）用单独的数据目录，也不该和正式版互斥。
fn lock_path() -> PathBuf {
    let tag = if std::env::args().any(|a| a == "--selftest") { "selftest" } else { "main" };
    // SAFETY: getuid 只读当前进程的凭证，没有副作用。
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/pikaclean-{uid}-{tag}.lock"))
}

/// 拿到锁返回 Some（调用方要把它保存到进程结束），已经有别的实例在跑返回 None。
///
/// 锁由内核持有，进程退出（包括崩溃、被 kill）时自动释放，所以不需要删文件。
/// 反过来说也不该删：删掉会让下一个实例在旧文件 inode 和新文件之间各锁各的。
pub fn acquire() -> Option<File> {
    let path = lock_path();
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(&path).ok()?;
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试默认并行跑，而这里几条都要独占那把锁，串起来避免互相抢。
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn 锁文件按用户和模式分开() {
        let name = lock_path();
        let name = name.to_string_lossy();
        assert!(name.starts_with("/tmp/pikaclean-"), "{name}");
        assert!(name.ends_with("-main.lock") || name.ends_with("-selftest.lock"), "{name}");
    }

    #[test]
    fn 第二个文件句柄拿不到同一把锁() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let file = acquire().expect("第一个实例应该拿到锁");
        // 另开一个句柄模拟另一个进程：flock 锁的是打开的文件，不是进程，所以同一进程也会被挡住。
        let other = OpenOptions::new().create(true).truncate(false).write(true).open(lock_path()).unwrap();
        assert!(other.try_lock().is_err(), "同一把锁不该被两个句柄同时持有");
        drop(file);
        assert!(other.try_lock().is_ok(), "上一个持有者释放后应该能拿到");
    }

    /// 真的起第二个进程来验证：登录时那两份是两个独立的进程，同进程内的句柄互斥不算数。
    /// 子进程复用本测试名，进到函数里看到环境变量就直接按能不能拿到锁退出。
    #[test]
    fn 第二个进程拿不到锁() {
        if std::env::var_os("PIKA_LOCK_CHILD").is_some() {
            // 拿到锁说明没互斥住（1）；拿不到说明互斥生效（0）
            std::process::exit(if acquire().is_some() { 1 } else { 0 });
        }
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let held = acquire().expect("父进程应该拿到锁");
        let exe = std::env::current_exe().expect("拿得到测试程序路径");
        let status = std::process::Command::new(&exe)
            .args(["--exact", "single_instance::tests::第二个进程拿不到锁", "--nocapture"])
            .env("PIKA_LOCK_CHILD", "1")
            .status()
            .expect("能起子进程");
        assert_eq!(status.code(), Some(0), "子进程不该拿到已被父进程持有的锁");
        drop(held);
        // 父进程放开后，新起的进程应该能拿到
        let status = std::process::Command::new(&exe)
            .args(["--exact", "single_instance::tests::第二个进程拿不到锁", "--nocapture"])
            .env("PIKA_LOCK_CHILD", "1")
            .status()
            .expect("能起子进程");
        assert_eq!(status.code(), Some(1), "锁释放后子进程应该能拿到");
    }
}
