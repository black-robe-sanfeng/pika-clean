import { type CSSProperties, Fragment, type ReactNode, useEffect, useRef, useState } from 'react';
import { type AdminError, api, type DeviceInfo, type JournalSummary, type Proc, type RemoveResult, type Snapshot, useAiAvailable, useSnapshot } from '../api';
import { AiAnswer } from '../components/AiAnswer';
import { Icon, type IconName } from '../components/Icon';
import { bytes, DISK_WARN_PCT, gb, mem, pct, rate, uptime } from '../lib/fmt';
import { health } from '../lib/health';
import { tint } from '../lib/tint';
import { useArmed } from '../lib/useArmed';
import { t } from '../lib/i18n.ts';

const CPU_POINTS = 12;
const NET_POINTS = 30;
const HOT_CPU = 10;
/** 柱状图在这个 CPU 占用时顶满。按 100% 画的话，日常的十几个百分点只剩一条线。 */
const CPU_BAR_FULL = 50;


const p = (value: number) => ({ '--p': value }) as CSSProperties;

function Tile(props: { icon: IconName; title: string; chip?: string; big: ReactNode; viz: ReactNode; foot: ReactNode; dim?: boolean }) {
  return (
    <div className={`tile${props.dim ? ' dim' : ''}`}>
      <div className="tt">
        <span>
          <Icon name={props.icon} />
          {props.title}
        </span>
        {props.chip && <span className="chip">{props.chip}</span>}
      </div>
      <div className="big">{props.big}</div>
      {props.viz}
      <small>{props.foot}</small>
    </div>
  );
}

const Meter = ({ value, color }: { value: number; color: string }) => (
  <div className="viz">
    <div className="meter" style={{ ...p(value), '--c': `var(--${color})` } as CSSProperties}>
      <i />
    </div>
  </div>
);

function Spark({ values, color }: { values: number[]; color: string }) {
  const max = Math.max(1, ...values);
  const step = 290 / (NET_POINTS - 1);
  const points = values.map((v, i) => `${(i * step).toFixed(1)},${(31 - (v / max) * 28).toFixed(1)}`).join(' ');
  return (
    <svg className="spark" viewBox="0 0 290 34" preserveAspectRatio="none" aria-hidden="true" style={{ stroke: `var(--${color})` }}>
      <polyline points={points} />
    </svg>
  );
}

function Tiles({ s, cpuHist, netHist }: { s: Snapshot; cpuHist: number[]; netHist: number[] }) {
  const [down, downUnit] = rate(s.net.down_bps);
  const [up, upUnit] = rate(s.net.up_bps);
  const diskPct = pct(s.disk.used_bytes, s.disk.total_bytes);
  return (
    <div className="t3">
      <Tile
        icon="chip"
        title="CPU"
        chip={s.cpu.temp_c == null ? undefined : `${Math.round(s.cpu.temp_c)}°C`}
        big={`${Math.round(s.cpu.usage_pct)}%`}
        viz={
          <div className="bars" aria-hidden="true">
            {Array.from({ length: CPU_POINTS }, (_, i) => (
              // 用位移代替改高度，过渡交给合成器做；最低留 2 像素高（柱高 18 像素的约 11%）
              <i key={i} style={{ transform: `translateY(${100 - Math.max(11, Math.min(100, ((cpuHist[cpuHist.length - CPU_POINTS + i] ?? 0) / CPU_BAR_FULL) * 100))}%)` }} />
            ))}
          </div>
        }
        foot={t('负载 {0} · {1} 核', s.cpu.load1.toFixed(1), s.cpu.cores)}
      />
      <Tile
        icon="gpu"
        title="GPU"
        dim={!s.gpu}
        chip={s.gpu?.temp_c == null ? undefined : `${Math.round(s.gpu.temp_c)}°C`}
        big={s.gpu ? `${Math.round(s.gpu.usage_pct)}%` : '—'}
        viz={<Meter value={s.gpu?.usage_pct ?? 0} color="orange" />}
        foot={s.gpu ? t('图形处理器') : t('读不到')}
      />
      <Tile
        icon="mem"
        title={t('内存')}
        chip={`${gb(s.mem.total_bytes, 0)} GB`}
        big={`${pct(s.mem.used_bytes, s.mem.total_bytes)}%`}
        viz={<Meter value={pct(s.mem.used_bytes, s.mem.total_bytes)} color={s.mem.pressure === 'normal' ? 'green' : 'orange'} />}
        foot={
          // 总量已在右上角标签里，有交换空间时省掉它，好让一行放得下
          s.mem.swap_total_bytes > 0
            ? t('{0} GB · 交换 {1} GB', gb(s.mem.used_bytes), gb(s.mem.swap_used_bytes))
            : `${gb(s.mem.used_bytes)} / ${gb(s.mem.total_bytes, 0)} GB`
        }
      />
      <Tile
        icon="drive"
        title={t('磁盘')}
        chip={bytes(s.disk.total_bytes)}
        big={`${diskPct}%`}
        viz={<Meter value={diskPct} color={diskPct >= DISK_WARN_PCT ? 'orange' : 'blue'} />}
        foot={
          <span style={{ display: 'flex', justifyContent: 'space-between' }}>
            <span>{t('可用 {0}', bytes(s.disk.total_bytes - s.disk.used_bytes))}</span>
            {s.disk.health_pct != null && <span title={t('固态硬盘按已用寿命算出的健康度')}>{t('健康 {0}%', s.disk.health_pct)}</span>}
          </span>
        }
      />
      <Tile
        icon="globe"
        title={t('网络')}
        chip={s.net.iface || t('未连接')}
        big={
          <>
            ↓ {down}
            <small>{downUnit}</small>
          </>
        }
        viz={<Spark values={netHist} color="teal" />}
        foot={`↑ ${up} ${upUnit}`}
      />
      {s.battery && (
        <Tile
          icon="batt"
          title={t('电池')}
          // 标题行放不下「健康 87%」：它会折到第二行，把整个面板顶高到 680 的上限以上。所以和原型一样，标签放电源状态，健康度放底下
          chip={s.battery.charging ? t('接电源') : t('用电池')}
          big={`${s.battery.pct}%`}
          viz={<Meter value={s.battery.pct} color={s.battery.pct <= 20 ? 'red' : 'green'} />}
          foot={s.battery.health_pct == null ? (s.battery.charging ? t('已接通电源') : t('使用电池')) : t('健康 {0}%', Math.round(s.battery.health_pct))}
        />
      )}
    </div>
  );
}

function Processes({ top }: { top: Proc[] }) {
  const [by, setBy] = useState<'mem' | 'cpu'>('mem');
  const ai = useAiAvailable();
  // 正在看 AI 解释的那个进程（按名称记，列表每 2 秒刷新也不会丢）
  const [asked, setAsked] = useState<string | null>(null);
  const quit = useArmed();
  const [note, setNote] = useState('');
  const val = (r: Proc) => (by === 'mem' ? r.mem_bytes : r.cpu_pct);
  let rows = [...top].sort((a, b) => val(b) - val(a)).slice(0, 5);
  // 正在看解释的进程掉出前 5 时顶替第 5 行，不让解释框被卸掉（卸掉再挂上会自动再问一次 AI）
  const keep = asked != null && !rows.some((r) => r.name === asked) ? top.find((r) => r.name === asked) : undefined;
  if (keep) rows = [...rows.slice(0, 4), keep];
  return (
    <div className="tile">
      <div className="tt">
        <span>
          <Icon name="bars" />
          {t('高占用进程 Top 5')}
        </span>
        <div className="seg">
          <button className={by === 'mem' ? 'on' : ''} onClick={() => setBy('mem')}>
            {t('按内存')}
          </button>
          <button className={by === 'cpu' ? 'on' : ''} onClick={() => setBy('cpu')}>
            {t('按 CPU')}
          </button>
        </div>
      </div>
      <div className="pr h">
        <span />
        <span style={{ textAlign: 'left' }}>{ai ? t('点名称看 AI 解释') : ''}</span>
        <span />
        <span>{t('内存')}</span>
        <span>% CPU</span>
        <span />
      </div>
      {rows.map((r) => (
        <Fragment key={r.pid}>
          <div className="pr">
            <span className={`ic sm ${tint(r.name)}`}>{r.name.charAt(0).toUpperCase()}</span>
            {ai ? (
              <button className="pn" onClick={() => setAsked(asked === r.name ? null : r.name)} aria-expanded={asked === r.name}>
                <span>{r.name}</span>
                <Icon name="scan" />
              </button>
            ) : (
              <span className="pn">
                <span>{r.name}</span>
              </span>
            )}
            {/* 按第一名的比例画。按占整机算的话单个进程常只有几个百分点，横条几乎是空的 */}
            <div
              className="meter"
              aria-hidden="true"
              style={{ ...p(pct(val(r), val(rows[0]))), '--c': `var(--${by === 'mem' ? 'orange' : 'green'})` } as CSSProperties}
            >
              <i />
            </div>
            <span className={`num${by === 'mem' ? ' on' : ''}`}>{mem(r.mem_bytes)}</span>
            <span className={`num${by === 'cpu' ? ' on' : ''}${r.cpu_pct > HOT_CPU ? ' hot' : ''}`}>{r.cpu_pct.toFixed(1)}</span>
            <button
              className={`x${quit.armed === r.name ? ' armed' : ''}`}
              aria-label={quit.armed === r.name ? t('再点一次，退出 {0}', r.name) : t('退出 {0}', r.name)}
              onClick={() => {
                if (!quit.press(r.name)) return setNote('');
                api.processQuit(r.name).then(
                  () => setNote(t('已请 {0} 退出', r.name)),
                  (e) => setNote(t('{0}：{1}', r.name, e)),
                );
              }}
            >
              {quit.armed === r.name ? t('确认') : t('退出')}
            </button>
          </div>
          {asked === r.name && (
            <div className="px">
              <AiAnswer ask={(onDelta, fresh) => api.aiExplainProcess(r.name, onDelta, fresh)} caution={t('退出进程前请自行确认')} />
            </div>
          )}
        </Fragment>
      ))}
      {/* 「再点一次」的提示跟着待确认状态走，4 秒复位时一起消失 */}
      {(quit.armed || note) && (
        <small role="status" style={{ display: 'block', paddingTop: 4 }}>
          {quit.armed ? t('再点一次，退出 {0}', quit.armed) : note}
        </small>
      )}
    </div>
  );
}

/** 小工具和累计记录。 */
function Tools({ fan, mic, camera }: { fan: Snapshot['fan']; mic: boolean | null; camera: boolean | null }) {
  const [awake, setAwake] = useState(false);
  const [volumes, setVolumes] = useState<string[]>([]);
  const [sum, setSum] = useState<JournalSummary | null>(null);
  const [note, setNote] = useState('');
  // 键盘清洁：还要锁多少秒。0 表示没锁。后端到时间会自己解锁，这里只是同步倒数
  const [locked, setLocked] = useState(0);
  useEffect(() => {
    if (locked <= 0) return;
    const t = setTimeout(() => setLocked((n) => n - 1), 1000);
    return () => clearTimeout(t);
  }, [locked]);

  function toggleKeyboard() {
    if (locked > 0) {
      api.keyboardCleanStop();
      setLocked(0);
      return;
    }
    api.keyboardCleanStart(30).then(
      (seconds) => {
        setLocked(seconds);
        setNote(t('键盘已锁住，可以擦了。鼠标照常能用，到时间自动解锁'));
      },
      (e) => {
        if (e !== 'no_access') return setNote(t('没能锁住键盘'));
        setNote(t('锁键盘需要「辅助功能」权限，已为你打开系统设置，勾选本应用后再点一次'));
        api.openAccessibilitySettings();
      },
    );
  }

  const refresh = () => {
    api.awakeGet().then(setAwake);
    api.volumesList().then(setVolumes);
    api.journalSummary().then(setSum);
  };
  // 面板每次重新显示时刷新一遍：外接磁盘可能插拔过，主窗口里可能刚清理过
  useEffect(() => {
    refresh();
    const onShow = () => document.visibilityState === 'visible' && refresh();
    document.addEventListener('visibilitychange', onShow);
    window.addEventListener('focus', refresh);
    return () => {
      document.removeEventListener('visibilitychange', onShow);
      window.removeEventListener('focus', refresh);
    };
  }, []);

  async function ejectAll() {
    const failed: string[] = [];
    for (const v of volumes) await api.volumeEject(v).catch(() => failed.push(v));
    setNote(failed.length ? t('没能推出：{0}（可能有文件正在使用）', failed.join(t('、'))) : t('已推出'));
    api.volumesList().then(setVolumes);
  }

  return (
    <div className="tile" style={{ display: 'grid', gap: 10 }}>
      {fan && (
        <div className="line">
          <Icon name="fan" />
          {t('风扇')}
          <span className="num">{fan.rpm > 0 ? t('{0} 转/分', fan.rpm.toLocaleString()) : t('停转')}</span>
          <span className="r">{t('{0} 个 · 由系统调节', fan.count)}</span>
        </div>
      )}
      <div className="line">
        <button className={`tool${awake ? ' on' : ''}`} aria-pressed={awake} onClick={() => api.awakeSet(!awake).then(setAwake)}>
          <Icon name="awake" />
          {awake ? t('常亮已开，电脑不会休眠') : t('常亮')}
        </button>
        <button className={`tool${locked > 0 ? ' on' : ''}`} aria-pressed={locked > 0} onClick={toggleKeyboard} title={t('暂时锁住键盘，方便擦拭')}>
          <Icon name="scan" />
          {locked > 0 ? t('已锁 {0} 秒 · 点此解锁', locked) : t('清洁')}
        </button>
        <button className="tool r" disabled={volumes.length === 0} onClick={ejectAll} title={volumes.join(t('、'))}>
          <Icon name="eject" />
          {volumes.length === 0 ? t('没有外接磁盘') : t('推出 {0} 个外接磁盘', volumes.length)}
        </button>
      </div>
      {/* 只在正被使用时出现，和系统自己的指示灯一个思路；平时不占高度（面板默认高度上限 680） */}
      {(mic || camera) && (
        <div className="line">
          {mic && (
            <>
              <Icon name="mic" />
              <span className="live">{t('麦克风正在使用')}</span>
            </>
          )}
          {camera && (
            <>
              <Icon name="cam" />
              <span className="live">{t('摄像头正在使用')}</span>
            </>
          )}
        </div>
      )}
      <div className="line">
        <Icon name="trash" />
        {t('累计')}
        <span className="r">
          {t('已清理')}{' '}<span className="num">{bytes(sum?.cleaned_bytes ?? 0)}</span>
        </span>
        <span>
          {t('已卸载')}{' '}<span className="num">{sum?.uninstalled ?? 0}</span>
        </span>
        <span>
          {t('已优化')}{' '}<span className="num">{sum?.optimized ?? 0}</span>
        </span>
      </div>
      {note && <small role="status">{note}</small>}
    </div>
  );
}

export function PopoverPage() {
  const snap = useSnapshot();
  const [device, setDevice] = useState<DeviceInfo | null>(null);
  const content = useRef<HTMLDivElement>(null);
  const armed = useArmed();
  const [purge, setPurge] = useState('');
  // 授权过免密就不用「再点一次」了。面板每次重新显示时重读：授权是在主窗口里开关的
  const [noPassword, setNoPassword] = useState(false);
  useEffect(() => {
    const load = () => document.visibilityState === 'visible' && api.adminPasswordless().then((p) => setNoPassword(p.on && p.tasks.includes('purge_memory')));
    load();
    document.addEventListener('visibilitychange', load);
    return () => document.removeEventListener('visibilitychange', load);
  }, []);
  const score = snap ? health(snap) : null;
  // 快速清理：先算能腾出多少（checking），算好后（ready）再点一次才真清（busy）；结果和待确认都过几秒复原
  const [quick, setQuick] = useState('');
  const [quickBytes, setQuickBytes] = useState(0);
  useEffect(() => {
    if (!quick || quick === 'checking' || quick === 'busy') return;
    const timer = setTimeout(() => setQuick(''), quick === 'ready' ? 8000 : 4000);
    return () => clearTimeout(timer);
  }, [quick]);
  const quickClean = () => {
    const sum = (r: RemoveResult) => r.freed_bytes + r.trashed_bytes;
    const fail = (e: unknown) => setQuick(String(e));
    if (quick === 'ready') {
      setQuick('busy');
      api.quickClean(false).then((r) => setQuick(t('已清理 {0}', bytes(sum(r)))), fail);
      return;
    }
    setQuick('checking');
    api.quickClean(true).then((r) => {
      setQuickBytes(sum(r));
      setQuick(sum(r) < 1 << 20 ? t('没什么可清理的') : 'ready');
    }, fail);
  };

  useEffect(() => {
    api.deviceInfo().then(setDevice);
  }, []);

  // 释放内存的结果过几秒复原成按钮原文；再次点击或面板卸载时清掉旧定时器
  useEffect(() => {
    if (!purge || purge === 'busy') return;
    const timer = setTimeout(() => setPurge(''), 4000);
    return () => clearTimeout(timer);
  }, [purge]);

  // 窗口高度跟随内容：没有电池的机器少一张卡片，面板就该矮一截
  useEffect(() => {
    const el = content.current!;
    const ro = new ResizeObserver(() => api.popoverResize(el.offsetHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  return (
    <div className="pop">
      <div className="pop-in" ref={content}>
        <div className="hd" title={score ? (score.reasons.length ? t('扣分原因：{0}', score.reasons.join(t('；'))) : t('各项正常')) : undefined}>
          {score && <span className="num">{score.score}</span>}
          <b>{score ? score.label : t('系统状态')}</b>
          {(device || snap) && (
            <span className="chip">
              {[device?.chip.replace(/^Apple /, ''), device && `${gb(device.mem_total_bytes, 0)} GB`, device && `macOS ${device.os_version}`, snap && uptime(snap.uptime_s)].filter(Boolean).join(' · ')}
            </span>
          )}
        </div>

        {snap ? <Tiles s={snap} cpuHist={snap.cpu_hist} netHist={snap.net_hist} /> : <div className="tile">{t('正在读取系统状态…')}</div>}

        {snap && <Processes top={snap.top} />}

        <Tools fan={snap?.fan ?? null} mic={snap?.mic_in_use ?? null} camera={snap?.camera_in_use ?? null} />

        <div className="pbtns">
          <button
            className="btn gray"
            disabled={purge === 'busy'}
            onClick={() => {
              // 先说明要输密码，再点一次才弹系统密码框
              if (!noPassword && !armed.press('purge')) return;
              setPurge('busy');
              // 前后各量一次空闲内存，如实显示腾出了多少（文件缓存清掉后通常不多，真要腾内存得去主窗口退出应用）
              api.memFree().then((before) =>
                api.adminRun('purge_memory').then(
                  () =>
                    new Promise((r) => setTimeout(r, 1000))
                      .then(() => api.memFree())
                      .then((after) => setPurge(before != null && after != null ? t('腾出 {0}', mem(Math.max(0, after - before))) : t('已释放'))),
                  (e: AdminError) => setPurge(e.kind === 'cancelled' ? '' : t('没成功：{0}', e.message ?? '')),
                ),
              );
            }}
            title={purge && purge !== 'busy' ? purge : undefined}
          >
            {purge === 'busy' ? t('正在释放…') : armed.armed === 'purge' ? t('要输管理员密码，再点一次') : purge || t('释放内存')}
          </button>
          <button className="btn gray" disabled={quick === 'checking' || quick === 'busy'} onClick={quickClean} title={t('清掉缓存、日志和废纸篓')}>
            {quick === 'checking'
              ? t('正在计算…')
              : quick === 'busy'
                ? t('正在清理…')
                : quick === 'ready'
                  ? t('清掉 {0}？再点一次', bytes(quickBytes))
                  : quick || t('快速清理')}
          </button>
          {/* 不指定页面：主窗口停在用户上次看的那页，和菜单栏右键菜单的「打开主窗口」一样 */}
          <button className="btn" onClick={() => api.showMain()}>
            {t('打开主窗口')}
          </button>
        </div>
      </div>
    </div>
  );
}
