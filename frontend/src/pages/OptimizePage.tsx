import { type CSSProperties, Fragment, type ReactNode, useEffect, useRef, useState } from 'react';
import { type AdminError, type AdminTask, api, type LoginItem, type LoginItems, type Passwordless, type Proc, useAiAvailable, useSnapshot } from '../api';
import { Check } from '../components/Check';
import { AiAnswer } from '../components/AiAnswer';
import { Dialog } from '../components/Dialog';
import { Sheet } from '../components/Sheet';
import { Icon, type IconName, Tile } from '../components/Icon';
import { toast } from '../components/Toast';
import { gb, mem, pct } from '../lib/fmt';
import { tint } from '../lib/tint';
import { useArmed } from '../lib/useArmed';
import { t } from '../lib/i18n.ts';

const PRESSURE = { normal: t('正常'), warn: t('偏高'), critical: t('紧张') } as const;

/** 需要管理员密码的任务。说明文字要让用户在输密码之前就知道它会做什么。 */
const TASKS: { id: AdminTask; name: string; desc: string; icon: IconName; color: string }[] = [
  { id: 'flush_dns', name: t('刷新 DNS 缓存'), desc: t('网页打不开或解析变慢时使用'), icon: 'globe', color: 'c-teal' },
  { id: 'rebuild_spotlight', name: t('重建聚焦搜索索引'), desc: t('搜索结果不全时使用'), icon: 'doc', color: 'c-gray' },
  { id: 'thin_snapshots', name: t('回收可清除空间'), desc: t('清掉系统保留的本地快照'), icon: 'drive', color: 'c-indigo' },
];

/** 内存卡片自己订阅快照：每次采样只重画这一块，不带着整页重渲染 */
function MemoryCard({ active, status, running, onPurge }: { active: boolean; status?: string; running: boolean; onPurge: () => void }) {
  const snap = useSnapshot(active);
  const memPct = snap ? pct(snap.mem.used_bytes, snap.mem.total_bytes) : 0;
  return (
    <div className="group mem">
      <div className="gauge xl" style={{ '--p': memPct, '--c': snap?.mem.pressure === 'normal' ? 'var(--green)' : 'var(--orange)' } as CSSProperties}>
        <i />
        <b>{memPct}%</b>
      </div>
      <div className="grow" style={{ gap: 6 }}>
        <span className="num">{snap ? `${gb(snap.mem.used_bytes)} GB` : '—'}</span>
        <small>{snap ? t('共 {0} GB', gb(snap.mem.total_bytes, 0)) : '—'}</small>
        <small>{snap ? t('压力{0}', PRESSURE[snap.mem.pressure]) : ''}</small>
        {status && <small role="status">{status}</small>}
      </div>
      <button className="btn gray sm" disabled={running} onClick={onPurge}>
        {t('释放内存')}
      </button>
    </div>
  );
}

/** 退出也腾不出内存、或者退了会出问题的：系统自己的进程和本应用 */
const KEEP = new Set(['Finder', 'Dock', 'SystemUIServer', 'WindowServer', 'loginwindow', 'ControlCenter', 'Spotlight', 'kernel_task', 'Pika Clean', 'pikaclean']);
const PURGE_KEY = '__purge__';

/**
 * 释放内存：列出占内存多的应用，勾选后请它们退出；可以顺带清一下文件缓存（purge）。
 * 前后各量一次空闲内存，如实告诉用户腾出了多少。purge 单独用效果很有限，真正腾出内存的是退出应用。
 */
function MemorySheet({ open, noPassword, onClose, onDone }: { open: boolean; noPassword: boolean; onClose: () => void; onDone: (msg: string) => void }) {
  const snap = useSnapshot(open);
  // 打开时把列表定住：快照每 2 秒刷新一次，跟着变的话勾着勾着行就跳了
  const [apps, setApps] = useState<Proc[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  useEffect(() => {
    if (!open) return setApps(null);
    if (apps || !snap) return;
    setApps(snap.top.filter((p) => !KEEP.has(p.name) && p.mem_bytes >= 100 * 1024 * 1024).slice(0, 10));
    // 文件缓存免密时默认勾上，要输密码时不勾，免得一点就弹密码框
    setPicked(new Set(noPassword ? [PURGE_KEY] : []));
  }, [open, snap, apps, noPassword]);
  const toggle = (key: string) =>
    setPicked((p) => {
      const next = new Set(p);
      next.has(key) ? next.delete(key) : next.add(key);
      return next;
    });
  const chosen = (apps ?? []).filter((a) => picked.has(a.name));
  const run = async () => {
    const before = await api.memFree().catch(() => null);
    let stuck: string[] = [];
    if (chosen.length) {
      setBusy(t('正在请应用退出…'));
      stuck = await api.appsQuit(chosen.map((a) => a.name)).catch((e) => {
        toast(String(e));
        return chosen.map((a) => a.name);
      });
    }
    if (picked.has(PURGE_KEY)) {
      setBusy(noPassword ? t('正在清理文件缓存…') : t('等你在系统密码框里输入密码…'));
      await api.adminRun('purge_memory').catch((e: AdminError) => e.kind !== 'cancelled' && toast(t('没成功：{0}', e.message ?? '')));
    }
    // 应用刚退出时，系统回收它的内存要一小会儿
    await new Promise((r) => setTimeout(r, 1500));
    const after = await api.memFree().catch(() => null);
    setBusy(null);
    const done = before != null && after != null ? t('实际腾出 {0}', mem(Math.max(0, after - before))) : t('已完成');
    const msg = stuck.length ? t('{0}；{1} 没有退出，可能有没保存的内容', done, stuck.join('、')) : done;
    toast(msg);
    onDone(msg);
    onClose();
  };
  return (
    <Sheet
      open={open}
      title={t('释放内存')}
      onClose={() => !busy && onClose()}
      footer={
        <div style={{ display: 'flex', alignItems: 'center', gap: 12 }}>
          <span className="grow">{chosen.length ? t('退出 {0} 个应用，预计腾出约 {1}', chosen.length, mem(chosen.reduce((s, a) => s + a.mem_bytes, 0))) : t('勾选要退出的应用')}</span>
          <button className="btn" disabled={busy != null || picked.size === 0} onClick={run}>
            {busy ?? t('释放')}
          </button>
        </div>
      }
    >
      <p>{t('真正能腾出内存的是退出占得多的应用。这里只是请应用自己退出，和按 ⌘Q 一样，有没保存的内容时它会先问你。')}</p>
      <div className="gh">{t('退出这些应用')}</div>
      <div className="group" data-empty={apps == null ? t('正在读取…') : t('没有占内存多的应用')}>
        {(apps ?? []).map((a) => (
          <label className="row" key={a.name}>
            <span className={`ic ${tint(a.name)}`} aria-hidden="true">
              {a.name.charAt(0).toUpperCase()}
            </span>
            <span className="grow">
              <span>{a.name}</span>
              <small style={{ whiteSpace: 'normal' }}>{/docker/i.test(a.name) ? t('大部分是它的虚拟机；在 Docker 设置的「资源」里调低内存上限，能长期少占') : t('退出可腾出约 {0}', mem(a.mem_bytes))}</small>
            </span>
            <span className="num">{mem(a.mem_bytes)}</span>
            <Check all={picked.has(a.name)} disabled={busy != null} onChange={() => toggle(a.name)} aria-label={a.name} />
          </label>
        ))}
      </div>
      <div className="gh">{t('系统')}</div>
      <div className="group">
        <label className="row">
          <span className="grow">
            <span>{t('清理文件缓存')}</span>
            <small>{noPassword ? t('系统本来就会自己回收这部分，效果通常有限') : t('系统本来就会自己回收这部分，效果通常有限 · 要输管理员密码')}</small>
          </span>
          <Check all={picked.has(PURGE_KEY)} disabled={busy != null} onChange={() => toggle(PURGE_KEY)} aria-label={t('清理文件缓存')} />
        </label>
      </div>
    </Sheet>
  );
}

/** 占用最多的进程，同样自己订阅快照 */
function TopProcesses({ active }: { active: boolean }) {
  const snap = useSnapshot(active);
  const quit = useArmed();
  return (
    <div className="group" data-empty={t('正在读取…')}>
      {(snap?.top ?? []).slice(0, 5).map((p) => (
        <div className="row" key={p.pid}>
          <span className={`ic ${tint(p.name)}`} aria-hidden="true">
            {p.name.charAt(0).toUpperCase()}
          </span>
          <span className="grow">
            <span>{p.name}</span>
            <small>CPU {p.cpu_pct.toFixed(1)}%</small>
          </span>
          <span className="num">{mem(p.mem_bytes)}</span>
          <button
            className={`btn sm ${quit.armed === p.name ? 'red' : 'gray'}`}
            onClick={() => quit.press(p.name) && api.processQuit(p.name).then(() => toast(t('已请 {0} 退出', p.name)), (e) => toast(t('{0}：{1}', p.name, e)))}
          >
            {quit.armed === p.name ? t('确认退出') : t('退出')}
          </button>
        </div>
      ))}
    </div>
  );
}

export function OptimizePage({ active }: { active: boolean }) {
  const ai = useAiAvailable();
  const [logins, setLogins] = useState<LoginItems | null>(null);
  const [loginsOpen, setLoginsOpen] = useState(false);
  const [pending, setPending] = useState<{ id: AdminTask; name: string; desc: string } | null>(null);
  const [status, setStatus] = useState<Partial<Record<AdminTask, string>>>({});
  const [free, setFree] = useState<Passwordless | null>(null);
  const [remember, setRemember] = useState(false);
  const [diagnose, setDiagnose] = useState(0);
  const [memOpen, setMemOpen] = useState(false);
  // 正在执行的维护任务：执行中按钮不能再点，免得连点两次跑两遍
  const [running, setRunning] = useState<Partial<Record<AdminTask, boolean>>>({});

  // 应用常驻菜单栏，这一页可能停留很久：每次切回来，距上次读超过 30 秒就重读启动项
  const lastLoad = useRef(0);
  useEffect(() => {
    if (!active || (logins != null && Date.now() - lastLoad.current < 30_000)) return;
    lastLoad.current = Date.now();
    api.loginItems().then(setLogins, () => setLogins((old) => old ?? { items: [], apps_unreadable: false }));
  }, [active]);
  const list = logins?.items;

  // 每次进入都重新读：免密授权可能刚在设置里开关过
  const loadFree = () => api.adminPasswordless().then(setFree);
  useEffect(() => {
    if (active) loadFree();
  }, [active]);
  const noPassword = (task: AdminTask) => !!free?.on && free.tasks.includes(task);
  /** 授权过免密的任务直接执行，其余的先弹框说明要输密码。 */
  const start = (task: { id: AdminTask; name: string; desc: string }) => {
    if (noPassword(task.id)) return run(task.id);
    setRemember(false);
    setPending(task);
  };

  function run(task: AdminTask, grant = false) {
    setPending(null);
    setRunning((r) => ({ ...r, [task]: true }));
    setStatus((s) => ({ ...s, [task]: noPassword(task) ? t('正在执行…') : t('等待输入密码…') }));
    // 勾了「以后不再询问」就先授权（只弹这一次密码框），任务本身随后免密执行
    const granted = grant ? api.adminPasswordlessSet(true).then(loadFree) : Promise.resolve();
    granted.then(() => api.adminRun(task)).then(
      () => setStatus((s) => ({ ...s, [task]: task === 'purge_memory' ? t('已释放内存') : t('已完成') })),
      (e: AdminError) => setStatus((s) => ({ ...s, [task]: e.kind === 'cancelled' ? '' : t('没成功：{0}', e.message ?? '') })),
    ).finally(() => setRunning((r) => ({ ...r, [task]: false })));
  }

  return (
    <>
      <div className="scroll">
        <h1>{t('优化速度')}</h1>
        <p>{t('释放内存、管理开机启动项、结束占用资源的进程')}</p>
        {ai && (
          <div style={{ marginBottom: 14, display: 'grid', gap: 10, justifyItems: 'start' }}>
            <button className="pill" onClick={() => setDiagnose((n) => n + 1)}>
              <Icon name="scan" />
              {diagnose ? t('重新诊断') : t('为什么我的 Mac 变慢了？')}
            </button>
            {diagnose > 0 && (
              <div style={{ justifySelf: 'stretch' }}>
                <AiAnswer key={diagnose} ask={api.aiDiagnose} caution={t('动手前请自行确认')} />
              </div>
            )}
          </div>
        )}
        <div className="cols optimize-grid">
          <div className="opt-column">
            <div className="opt-section opt-memory">
              <div className="gh">{t('内存')}</div>
              <MemoryCard active={active} status={status.purge_memory} running={!!running.purge_memory} onPurge={() => {
                  setLoginsOpen(false);
                  setMemOpen(true);
                }} />
            </div>
            <div className="opt-section opt-tasks">
              <div className="gh">{t('维护任务 · 需要管理员密码')}</div>
              <div className="group">
                {TASKS.map((task) => (
                  <div className="row" key={task.id}>
                    <Tile name={task.icon} color={task.color} />
                    <span className="grow">
                      <span>{task.name}</span>
                      <small>{status[task.id] || task.desc}</small>
                    </span>
                    <button className="btn gray sm" disabled={!!running[task.id]} onClick={() => start(task)}>
                      {t('运行')}
                    </button>
                  </div>
                ))}
              </div>
            </div>
          </div>

          <div className="opt-column">
            <div className="opt-section opt-login">
              <div className="gh">{t('开机自动启动 · {0}', list == null ? t('读取中') : t('已启用 {0} / {1} 项', list.filter((l) => l.enabled).length, list.length))}</div>
              <div className="group" data-empty={list == null ? t('正在读取…') : t('没有发现开机启动项')}>
                {(list ?? []).slice(0, 4).map((l) => (
                  <LoginRow key={`${l.kind}:${l.name}`} item={l} />
                ))}
                {list != null && (
                  <button
                    className="row"
                    onClick={() => {
                      setMemOpen(false);
                      setLoginsOpen(true);
                    }}
                  >
                    <span className="grow">
                      <span>{t('管理开机启动项{0}', list.length > 4 ? t('（共 {0} 项）', list.length) : '')}</span>
                      <small>{t('启用、停用、移除或添加')}</small>
                    </span>
                    <Icon name="chev" className="chev" />
                  </button>
                )}
              </div>
            </div>
            <div className="opt-section opt-processes">
              <div className="gh">{t('占用资源最多的进程 · 含被压缩和换出到磁盘的内存')}</div>
              <TopProcesses active={active} />
            </div>
          </div>
        </div>
      </div>

      <MemorySheet open={memOpen} noPassword={noPassword('purge_memory')} onClose={() => setMemOpen(false)} onDone={(msg) => setStatus((s) => ({ ...s, purge_memory: msg }))} />
      <LoginSheet open={loginsOpen} items={list ?? []} appsUnreadable={!!logins?.apps_unreadable} onClose={() => setLoginsOpen(false)} reload={() => api.loginItems().then(setLogins)} />

      {pending && (
        <Dialog
          title={t('{0}？', pending.name)}
          actions={
            <>
              <button className="btn gray" onClick={() => setPending(null)}>
                {t('取消')}
              </button>
              <button className="btn" onClick={() => run(pending.id, remember)}>
                {t('继续')}
              </button>
            </>
          }
        >
          <p>{t('{0}。', pending.desc)}</p>
          <p>{t('这项操作需要管理员权限。点「继续」后系统会弹出密码框，密码由系统处理，本应用看不到。')}</p>
          {free && !free.on && free.tasks.includes(pending.id) && (
            <label style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
              <input className="ck" type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} />
              {t('以后释放内存和刷新 DNS 时不再询问密码')}
            </label>
          )}
        </Dialog>
      )}
    </>
  );
}

const kindName = (l: LoginItem) => (l.kind === 'login_item' ? t('登录时打开') : l.kind === 'daemon' ? t('系统级后台服务') : l.global ? t('后台常驻 · 所有用户') : t('后台常驻'));

function LoginRow({ item, children }: { item: LoginItem; children?: ReactNode }) {
  const shown = item.app ?? item.name;
  return (
    <div className="row" title={[item.name, item.path, item.program].filter(Boolean).join('\n')}>
      {/* 停用的只淡化名字，开关和按钮保持正常亮度 */}
      <span className={`ic ${tint(shown)}`} aria-hidden="true" style={item.enabled ? undefined : { opacity: 0.55 }}>
        {shown.charAt(0).toUpperCase()}
      </span>
      <span className="grow" style={item.enabled ? undefined : { opacity: 0.55 }}>
        <span>
          {shown}
          {/* 程序已经不在了，每次登录都会启动失败 */}
          {item.broken && <em className="tag">{t('程序已不存在')}</em>}
        </span>
        {/* 认出了所属应用时，标签（如 com.docker.vmnetd）放到小字里 */}
        <small>
          {item.enabled ? kindName(item) : t('{0} · 已停用', kindName(item))}
          {item.app && ` · ${item.name}`}
        </small>
      </span>
      {children}
    </div>
  );
}

/** 开机启动项的完整列表：每项能开关，大多数能移除；底部能添加应用。系统级后台服务只列出来看。 */
function LoginSheet({ open, items, appsUnreadable, onClose, reload }: { open: boolean; items: LoginItem[]; appsUnreadable: boolean; onClose: () => void; reload: () => Promise<void> }) {
  const [busy, setBusy] = useState<string | null>(null);
  const del = useArmed();
  const ai = useAiAvailable();
  // 展开了 AI 解释的那一项
  const [asked, setAsked] = useState<string | null>(null);
  const act = (key: string, job: Promise<unknown>) => {
    setBusy(key);
    job.catch((e) => toast(String(e))).then(reload).finally(() => setBusy(null));
  };
  const group = (title: string, list: LoginItem[]) =>
    list.length > 0 && (
      <>
        <div className="gh">{title}</div>
        <div className="group">
          {list.map((l) => {
            const key = `${l.kind}:${l.name}`;
            return (
              <Fragment key={key}>
                <LoginRow item={l}>
                  {ai && (
                    <button className="aib" onClick={() => setAsked(asked === key ? null : key)} aria-label={t('让 AI 解释 {0} 是什么', l.name)} aria-expanded={asked === key}>
                      <Icon name="scan" />
                    </button>
                  )}
                  {/* 系统级后台服务的开关和移除要管理员权限，这里只给看 */}
                  {l.kind !== 'daemon' && (
                    <input
                      className="sw"
                      type="checkbox"
                      checked={l.enabled}
                      disabled={busy != null}
                      aria-label={t('开机时启动 {0}', l.name)}
                      onChange={(e) => act(key, api.loginItemSet(l, e.target.checked))}
                    />
                  )}
                  {!l.global && (
                    // 平时只放图标，给名字留地方；点一下才展开成文字确认
                    <button
                      className={`btn sm ${del.armed === key ? 'red' : 'gray ico'}`}
                      disabled={busy != null}
                      aria-label={t('移除 {0}', l.name)}
                      onClick={() => del.press(key) && act(key, api.loginItemRemove(l))}
                    >
                      {del.armed === key ? t('确认移除') : <Icon name="trash" />}
                    </button>
                  )}
                </LoginRow>
                {asked === key && (
                  <div className="ai-in">
                    <AiAnswer ask={(onDelta, fresh) => api.aiExplainLoginItem(l, onDelta, fresh)} caution={t('动手前请自行确认')} />
                  </div>
                )}
              </Fragment>
            );
          })}
        </div>
      </>
    );
  return (
    <Sheet
      open={open}
      title={t('开机启动项')}
      onClose={onClose}
      footer={
        <div style={{ display: 'grid', gap: 8, justifyItems: 'start' }}>
          <button className="btn sm" disabled={busy != null} onClick={() => act('add', api.loginItemAdd())}>
            {t('添加应用…')}
          </button>
          <button className="link" onClick={() => api.openLoginItemsSettings().catch((e) => toast(String(e)))}>
            {t('应用自己注册的后台项目在系统设置里管理')}
          </button>
        </div>
      }
    >
      <p>{t('停用后下次登录不再自动启动，随时能再打开。移除启动代理会把它的配置文件移到废纸篓。')}</p>
      {appsUnreadable && <p role="status">{t('读不到登录时打开的应用：需要在系统设置的「隐私与安全性 › 自动化」里允许本应用控制「系统事件」。')}</p>}
      {group(t('登录时打开的应用'), items.filter((l) => l.kind === 'login_item'))}
      {group(t('后台常驻的启动代理'), items.filter((l) => l.kind === 'agent'))}
      {group(t('系统级后台服务 · 只能查看'), items.filter((l) => l.kind === 'daemon'))}
    </Sheet>
  );
}
