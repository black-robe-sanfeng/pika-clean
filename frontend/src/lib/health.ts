// 带上扩展名：这个文件也会被 node --test 直接加载
import type { Snapshot } from '../api/types.ts';
import { DISK_WARN_PCT, pct } from './fmt.ts';
import { t } from './i18n.ts';

export interface Health {
  score: number;
  label: string;
  /** 扣分的原因，没有扣分时为空 */
  reasons: string[];
}

/**
 * 健康评分：从 100 分往下扣。
 * 这套扣分规则是按规格 O-11 的推荐思路先定的，用户还没确认过，改起来只动这一个函数。
 */
export function health(s: Snapshot): Health {
  let score = 100;
  const reasons: string[] = [];
  const lose = (points: number, why: string) => {
    score -= Math.round(points);
    reasons.push(why);
  };

  const disk = pct(s.disk.used_bytes, s.disk.total_bytes);
  if (disk > DISK_WARN_PCT) lose(Math.min(40, (disk - DISK_WARN_PCT) * 2), t('磁盘已用 {0}%', disk));

  if (s.mem.pressure === 'warn') lose(15, t('内存压力偏高'));
  if (s.mem.pressure === 'critical') lose(30, t('内存压力紧张'));

  const load = s.cpu.cores > 0 ? s.cpu.load1 / s.cpu.cores : 0;
  if (load > 1) lose(Math.min(20, (load - 1) * 20), t('处理器负载高'));

  const battery = s.battery?.health_pct;
  if (battery != null && battery < 80) lose(10, t('电池健康度 {0}%', Math.round(battery)));

  score = Math.max(0, score);
  return { score, label: score >= 85 ? t('良好') : score >= 65 ? t('一般') : t('需要关注'), reasons };
}
