import { type CSSProperties, useEffect, useState } from 'react';
import { api, type UsageTrend, useAiAvailable, type WeekReport } from '../api';
import type { ScanState } from '../app/useScan';
import { AiAnswer } from '../components/AiAnswer';
import { Icon } from '../components/Icon';
import { CATEGORY } from '../lib/categories';
import { bytes } from '../lib/fmt';
import { t } from '../lib/i18n.ts';

/**
 * 结果列表顶上的 AI 洞察：这次扫描的总结（`summary` 为真时才给）。
 * AI 只在用户点击后才调用，不在后台自动调用；每来一份新报告，总结就收起。
 */
export function Insights({ scan, summary }: { scan: ScanState; summary: boolean }) {
  const ai = useAiAvailable();
  const [summarize, setSummarize] = useState(false);
  useEffect(() => setSummarize(false), [scan]);
  return (
    <div className="insights">
      {ai && summary && !summarize && (
        <button className="pill" onClick={() => setSummarize(true)}>
          <Icon name="scan" />
          {t('生成 AI 总结')}
        </button>
      )}
      {summarize && <AiAnswer ask={api.aiSummarizeScan} />}
    </div>
  );
}

/**
 * 用量趋势：最近几周每天多用多少、照这个速度几天后写满（数字在本机算，规格切片 14）。
 * 「AI 解释」点了才问（不变量 5）。
 */
function Trend() {
  const ai = useAiAvailable();
  const [trend, setTrend] = useState<UsageTrend | null | undefined>(undefined);
  const [explain, setExplain] = useState(false);
  useEffect(() => {
    api.usageTrend().then(setTrend, () => setTrend(null));
  }, []);
  if (trend === undefined) return null;
  if (trend === null)
    return (
      <div className="trend">
        <p>{t('用量趋势：再积累一两天的记录后显示')}</p>
      </div>
    );
  const days = trend.points[trend.points.length - 1].day - trend.points[0].day;
  const left = trend.days_to_full;
  const text =
    left == null
      ? t('近 {0} 天磁盘用量没有增加', days)
      : left > 365
        ? t('近 {0} 天每天多用约 {1}，按这个速度一年内写不满', days, bytes(trend.bytes_per_day))
        : t('近 {0} 天每天多用约 {1}，按这个速度约 {2} 天后写满', days, bytes(trend.bytes_per_day), left);
  return (
    <div className="trend">
      <p>
        {text}
        {ai && !explain && (
          <button className="link" onClick={() => setExplain(true)}>
            {t('AI 解释')}
          </button>
        )}
      </p>
      {explain && <AiAnswer ask={api.aiExplainTrend} />}
    </div>
  );
}

/** 近 7 天做了什么、磁盘多用了多少。数字全在本机算，不调用 AI；什么都没有时不显示 */
function Week() {
  const [week, setWeek] = useState<WeekReport | null>(null);
  useEffect(() => {
    api.weekReport().then(setWeek, () => setWeek(null));
  }, []);
  if (!week) return null;
  const { journal: j, disk_delta_bytes: d } = week;
  const parts = [
    j.cleaned_bytes > 0 && t('清理了 {0}', bytes(j.cleaned_bytes)),
    j.uninstalled > 0 && t('卸载了 {0} 个应用', j.uninstalled),
    d != null && (d >= 0 ? t('磁盘多用了 {0}', bytes(d)) : t('磁盘少用了 {0}', bytes(-d))),
  ].filter(Boolean);
  if (!parts.length) return null;
  return (
    <div className="trend">
      <p>{t('近 7 天：{0}', parts.join(' · '))}</p>
    </div>
  );
}

/** 还没有扫描结果时的样子：一个大按钮，第一次扫描时显示进度。有了结果之后这一页换成 CleanPage 的结果列表。 */
export function ScanPage({ scan, onScan, onCancel }: { scan: ScanState; onScan: () => void; onCancel: () => void }) {
  const progress = scan.phase === 'run' ? scan.progress : null;
  const percent = progress ? Math.round((progress.done / progress.total) * 100) : 0;
  return (
    <div className="scan" data-state={scan.phase}>
      <div className="s-idle">
        <button className="orb" onClick={onScan}>
          {t('扫描')}
        </button>
        <p>{t('检查缓存、日志、废纸篓和旧下载，找出可以腾出的空间')}</p>
        <Trend />
        <Week />
      </div>

      <div className="s-run">
        <div className="ring" style={{ '--p': percent } as CSSProperties}>
          <i />
          <b>{percent}%</b>
        </div>
        <h1>{t('正在扫描…')}</h1>
        <p>{progress ? t('正在检查{0}', CATEGORY[progress.category].name) : t('准备中')}</p>
        <button className="btn sm" style={{ marginTop: 10 }} onClick={onCancel}>
          {t('取消')}
        </button>
      </div>
    </div>
  );
}
