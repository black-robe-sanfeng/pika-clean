/** 退出也腾不出内存、或者退了会出问题的：系统自己的进程和本应用。后端 `system::KEEP` 也拦一道，两边保持一致 */
export const KEEP = new Set(['Finder', 'Dock', 'SystemUIServer', 'WindowServer', 'loginwindow', 'ControlCenter', 'Spotlight', 'kernel_task', 'Pika Clean', 'pikaclean']);
