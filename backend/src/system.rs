//! 菜单栏面板里的系统小工具：防休眠、推出外接磁盘、退出进程。

use crate::i18n::t;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;

/// 「常亮」：靠一个系统自带的防休眠进程实现，它活着电脑就不休眠。
#[derive(Default)]
pub struct Awake(Mutex<Option<Child>>);

impl Awake {
    pub fn is_on(&self) -> bool {
        let mut guard = self.0.lock().unwrap();
        // 进程被别人结束了就算关
        if guard.as_mut().is_some_and(|c| c.try_wait().ok().flatten().is_some()) {
            *guard = None;
        }
        guard.is_some()
    }

    pub fn set(&self, on: bool) -> Result<bool, String> {
        let mut guard = self.0.lock().unwrap();
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if on {
            // -d 屏幕不灭，-i 系统不因空闲休眠；-w 让它跟着本应用一起退出
            let child = Command::new("/usr/bin/caffeinate").args(["-d", "-i", "-w", &std::process::id().to_string()]).spawn().map_err(|e| e.to_string())?;
            *guard = Some(child);
        }
        Ok(on)
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        let _ = self.set(false);
    }
}

/// 可以推出的卷：/Volumes 下除启动盘以外的条目（启动盘在那里是一个指向 / 的链接）。
pub fn ejectable_volumes(volumes_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(volumes_dir) else { return vec![] };
    let mut names: Vec<String> = entries
        .flatten()
        // 用读目录时带回的类型，不去读挂载点本身：网络盘没响应时读它会一直卡住。符号链接的类型不算文件夹
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// 推出一个卷。`name` 必须是当前列表里的名字，不接受路径。
pub fn eject(volumes_dir: &Path, name: &str) -> Result<(), String> {
    if !ejectable_volumes(volumes_dir).iter().any(|v| v == name) {
        return Err(t("没有这个可推出的磁盘").into());
    }
    let target: PathBuf = volumes_dir.join(name);
    let out = Command::new("/usr/sbin/diskutil").arg("eject").arg(&target).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().chars().take(200).collect())
    }
}

/// 请一个进程退出（发终止信号，让它自己收尾）。不强杀。
pub fn quit(pid: u32) -> Result<(), String> {
    if pid <= 1 || pid == std::process::id() {
        return Err(t("不能退出这个进程").into());
    }
    // SAFETY: 只是向一个进程号发信号；进程不存在或无权限时系统返回错误。
    if unsafe { libc::kill(pid as i32, libc::SIGTERM) } == 0 {
        Ok(())
    } else {
        Err(match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) => t("没有权限退出这个进程").into(),
            Some(libc::ESRCH) => t("进程已经不在了").into(),
            _ => t("退出失败").into(),
        })
    }
}

/// 进程还在不在（没权限发信号也算在）。
pub fn alive(pid: u32) -> bool {
    // SAFETY: 信号 0 只检查进程是否存在，不发任何信号。
    unsafe { libc::kill(pid as i32, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::testutil::TempTree;

    #[test]
    fn 能分出进程在不在() {
        assert!(alive(std::process::id()));
        assert!(!alive(999_999));
    }

    #[test]
    fn 可推出的卷不包含启动盘的链接和隐藏项() {
        let t = TempTree::new();
        std::fs::create_dir_all(t.0.join("Volumes/移动硬盘")).unwrap();
        std::fs::create_dir_all(t.0.join("Volumes/.timemachine")).unwrap();
        std::os::unix::fs::symlink("/", t.0.join("Volumes/Macintosh HD")).unwrap();
        assert_eq!(ejectable_volumes(&t.0.join("Volumes")), ["移动硬盘"]);
        assert_eq!(eject(&t.0.join("Volumes"), "../etc"), Err("没有这个可推出的磁盘".into()));
        assert_eq!(eject(&t.0.join("Volumes"), "Macintosh HD"), Err("没有这个可推出的磁盘".into()));
    }

    #[test]
    fn 防休眠能开能关() {
        let awake = Awake::default();
        assert!(!awake.is_on());
        assert_eq!(awake.set(true), Ok(true));
        assert!(awake.is_on());
        assert_eq!(awake.set(false), Ok(false));
        assert!(!awake.is_on());
    }

    #[test]
    fn 退出进程的守卫和真实效果() {
        assert!(quit(0).is_err() && quit(1).is_err() && quit(std::process::id()).is_err());
        let mut child = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        assert_eq!(quit(child.id()), Ok(()));
        assert!(!child.wait().unwrap().success(), "收到终止信号后结束");
        assert_eq!(quit(child.id()), Err("进程已经不在了".into()));
    }
}
