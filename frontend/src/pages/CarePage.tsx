import { type CSSProperties, useEffect, useRef, useState } from 'react';
import { api, type CareReport, type Hog } from '../api';
import { toast } from '../components/Toast';
import { bytes, pct } from '../lib/fmt';
import { tint } from '../lib/tint';
import { useArmed } from '../lib/useArmed';
import { t } from '../lib/i18n.ts';

/** 累计读写量动辄上百 TB，bytes() 最大只到 GB */
const big = (n: number) => (n >= 1e12 ? `${(n / 1e12).toFixed(1)} TB` : bytes(n));

/** 健康度圈：80% 以上绿，往下橙 */
function Gauge({ value }: { value: number | null | undefined }) {
  return (
    <div className="gauge xl" style={{ '--p': value ?? 0, '--c': value == null || value >= 80 ? 'var(--green)' : 'var(--orange)' } as CSSProperties}>
      <i />
      <b>{value == null ? '—' : `${Math.round(value)}%`}</b>
    </div>
  );
}

function Tips({ list }: { list: string[] }) {
  return (
    list.length > 0 && (
      <div className="group">
        {list.map((tip) => (
          <div className="row" key={tip}>
            <small>{tip}</small>
          </div>
        ))}
      </div>
    )
  );
}

function BatteryCard({ r }: { r: CareReport }) {
  const b = r.battery;
  if (!b) return <div className="group" data-empty={t('这台 Mac 没有电池')} />;
  const tips: string[] = [];
  if (b.health_pct != null && b.health_pct < 80) tips.push(t('健康度低于 80%，续航会明显变短，可以考虑去售后更换电池。'));
  if (b.temp_c != null && b.temp_c >= 35) tips.push(t('电池温度偏高。高温最伤电池：别放在被子、软垫上用，充电时少跑大型游戏和渲染。'));
  if (b.plugged) tips.push(t('长期插着电源用时，在「系统设置 › 电池」里打开「优化电池充电」，能让电量多停在 80%，减缓老化。'));
  tips.push(t('尽量别让电量长期耗到 0% 或一直放着不用；长期不用时充到 50% 左右再收起来。'));
  return (
    <>
      <div className="group mem">
        <Gauge value={b.health_pct} />
        <div className="grow" style={{ gap: 6 }}>
          <span className="num">{b.cycles == null ? '—' : t('{0} 次循环', b.cycles)}</span>
          {b.design_cycles != null && <small>{t('设计寿命 {0} 次', b.design_cycles)}</small>}
          {b.max_mah != null && b.design_mah != null && <small>{t('充满 {0} mAh，出厂 {1} mAh', b.max_mah, b.design_mah)}</small>}
          <small>
            {[b.pct != null && `${b.pct}%`, b.charging ? t('正在充电') : b.plugged ? t('接着电源') : t('使用电池'), b.temp_c != null && `${b.temp_c.toFixed(1)}°C`].filter(Boolean).join(' · ')}
          </small>
        </div>
      </div>
      <div className="gh">{t('保养建议')}</div>
      <Tips list={tips} />
    </>
  );
}

function DiskCard({ r }: { r: CareReport }) {
  const { smart: s, used_bytes, total_bytes } = r.disk;
  const freePct = 100 - pct(used_bytes, total_bytes);
  const tips: string[] = [];
  if (total_bytes > 0 && freePct < 15) tips.push(t('剩余空间不到 15%。固态硬盘太满时写入变慢、磨损更快，建议用智能扫描清一清。'));
  if (s && s.health_pct < 80) tips.push(t('硬盘已用掉五分之一以上的寿命，记得用时间机器定期备份。'));
  if (s && s.spare_pct < 10) tips.push(t('备用块快用完了，请尽快备份重要数据。'));
  if (s && s.unsafe_shutdowns > 0) tips.push(t('有 {0} 次没有正常关机。尽量用菜单里的「关机」，少长按电源键强制关机。', s.unsafe_shutdowns));
  tips.push(t('少开占内存大的程序能减少交换文件写盘；「写盘最多的应用」一栏能找出写得多的那几个。'));
  return (
    <>
      <div className="group mem">
        <Gauge value={s?.health_pct} />
        <div className="grow" style={{ gap: 6 }}>
          <span className="num">{s ? big(s.written_bytes) : '—'}</span>
          <small>{s ? t('累计写入 · 累计读取 {0}', big(s.read_bytes)) : t('读不到硬盘健康信息')}</small>
          {s && <small>{t('通电 {0} 小时', s.power_on_hours)}</small>}
          <small>{[total_bytes > 0 && t('剩余 {0}%', freePct), s?.temp_c != null && `${s.temp_c}°C`].filter(Boolean).join(' · ')}</small>
        </div>
      </div>
      <div className="gh">{t('保养建议')}</div>
      <Tips list={tips} />
    </>
  );
}

/** label 为空时只画条：累计能耗的单位对用户没意义，看相对多少就够了 */
function HogList({ list, label, reload }: { list: Hog[]; label?: (h: Hog) => string; reload: () => void }) {
  const quit = useArmed();
  const max = Math.max(1, ...list.map((h) => h.value));
  return (
    <div className="group" data-empty={t('没有读到数据')}>
      {list.map((h) => (
        <div className="row" key={h.pid}>
          <span className={`ic ${tint(h.name)}`} aria-hidden="true">
            {h.name.charAt(0).toUpperCase()}
          </span>
          <span className="grow">
            <span>{h.name}</span>
            <div className="meter" style={{ '--p': pct(h.value, max) } as CSSProperties}>
              <i />
            </div>
          </span>
          {label && <span className="num">{label(h)}</span>}
          {h.app && (
            <button
              className={`btn sm ${quit.armed === h.name ? 'red' : 'gray'}`}
              onClick={() => quit.press(h.name) && api.careQuit(h.name).then(() => toast(t('已请 {0} 退出', h.name)), (e) => toast(t('{0}：{1}', h.name, e))).finally(() => setTimeout(reload, 1500))}
            >
              {quit.armed === h.name ? t('确认退出') : t('退出')}
            </button>
          )}
        </div>
      ))}
    </div>
  );
}

export function CarePage({ active }: { active: boolean }) {
  const [r, setR] = useState<CareReport | null>(null);
  // 和优化页一样：切回来时距上次读超过 30 秒就重读
  const last = useRef(0);
  const load = () => {
    last.current = Date.now();
    api.careReport().then(setR, (e) => toast(String(e)));
  };
  useEffect(() => {
    if (active && Date.now() - last.current >= 30_000) load();
  }, [active]);

  return (
    <div className="scroll care">
      <h1>{t('保养 Mac')}</h1>
      <p>{t('看看电池和硬盘的寿命，找出最耗电、写盘最多的应用')}</p>
      {!r ? (
        <div className="group" data-empty={t('正在读取…')} />
      ) : (
        <div className="cols optimize-grid">
          <div className="opt-column">
            <div className="opt-section">
              <div className="gh">{t('电池')}</div>
              <BatteryCard r={r} />
            </div>
            <div className="opt-section">
              <div className="gh">{t('硬盘')}</div>
              <DiskCard r={r} />
            </div>
          </div>
          <div className="opt-column">
            {r.energy.length > 0 && (
              <div className="opt-section">
                <div className="gh">{t('最耗电的应用 · 自启动以来')}</div>
                <HogList list={r.energy} reload={load} />
              </div>
            )}
            <div className="opt-section">
              <div className="gh">{t('写盘最多的应用 · 自启动以来')}</div>
              <HogList list={r.writes} label={(h) => big(h.value)} reload={load} />
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
