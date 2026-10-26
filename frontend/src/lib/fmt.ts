import { t } from './i18n.ts';
// 纯函数，前端唯一的数值格式化来源。测试见 fmt.test.ts。
const GB = 1024 ** 3;
const MB = 1024 ** 2;

/** 磁盘和文件大小：1.5 GB / 320 MB / 12 KB。和 Finder 一样按 1000 进位，用户拿 Finder 对得上。 */
export function bytes(n: number): string {
  return scaled(n, 1000);
}

/** 内存大小：和「活动监视器」一样按 1024 进位。 */
export function mem(n: number): string {
  return scaled(n, 1024);
}

function scaled(n: number, k: number): string {
  const [g, m] = [k ** 3, k ** 2];
  // 从 1000 MB 起就写成 GB，免得「1012 MB」排在「1.1 GB」和「965 MB」之间看着别扭
  if (n >= 1000 * m) return `${(n / g).toFixed(n >= 100 * g ? 0 : 1)} GB`;
  if (n >= m) return `${Math.round(n / m)} MB`;
  return `${Math.round(n / k)} KB`;
}

/** 内存的 GB 数，按 1024 进位。 */
export const gb = (n: number, digits = 1) => (n / GB).toFixed(digits);

/** 磁盘用到多满算「偏满」。侧栏、清理页、菜单栏面板用同一个标准。 */
export const DISK_WARN_PCT = 80;

export const pct = (part: number, total: number) => (total > 0 ? Math.round((part / total) * 100) : 0);

/** 网速：自动在 KB/s 和 MB/s 之间切换，返回 [数值, 单位]。 */
export function rate(bps: number): [string, string] {
  return bps >= MB ? [(bps / MB).toFixed(1), 'MB/s'] : [String(Math.round(bps / 1024)), 'KB/s'];
}

export function uptime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  if (d >= 1) return t('已开机 {0} 天', d);
  const h = Math.floor(seconds / 3600);
  return h >= 1 ? t('已开机 {0} 小时', h) : t('已开机 {0} 分钟', Math.max(1, Math.floor(seconds / 60)));
}

export function ago(ms: number | null, now = Date.now()): string {
  if (ms == null) return t('未知');
  const days = Math.floor((now - ms) / 86400000);
  if (days < 1) return t('今天');
  if (days < 30) return t('{0} 天前', days);
  if (days < 365) return t('{0} 个月前', Math.floor(days / 30));
  return t('{0} 年前', Math.floor(days / 365));
}
