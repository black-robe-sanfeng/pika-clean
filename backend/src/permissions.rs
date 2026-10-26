//! 系统权限的探测。

/// 是否有「完全磁盘访问」。做法：尝试打开一个只有授权后才能读的系统数据库。
pub fn has_full_disk_access() -> bool {
    std::env::var_os("HOME").is_some_and(|home| {
        std::fs::File::open(std::path::Path::new(&home).join("Library/Application Support/com.apple.TCC/TCC.db")).is_ok()
    })
}
