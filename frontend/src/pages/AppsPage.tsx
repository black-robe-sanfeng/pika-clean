import { Fragment, useEffect, useRef, useState } from 'react';
import { api, revealMenu, type AppInfo, type AppUpdate, type Leftover, type RemoveResult, type UninstallProgress, useAiAvailable } from '../api';
import { AiAnswer } from '../components/AiAnswer';
import { Dialog } from '../components/Dialog';
import { Icon } from '../components/Icon';
import { toast } from '../components/Toast';
import { ago, bytes } from '../lib/fmt';
import { tint } from '../lib/tint';
import { t } from '../lib/i18n.ts';

/** 多久没打开算「不常用」。 */
const STALE_DAYS = 120;
const BIG_BYTES = 1024 ** 3;

type Filter = 'all' | 'stale' | 'big' | 'new';

const isStale = (a: AppInfo) => a.last_used_ms != null && Date.now() - a.last_used_ms > STALE_DAYS * 86_400_000;
const total = (a: AppInfo) => a.size_bytes + (a.leftover_bytes ?? 0);
const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1);
/** 用户目录写成 ~，路径短一点 */
const shortPath = (path: string) => path.replace(/^\/Users\/[^/]+/, '~');
/** 卸载前会被请求退出：主程序开着，或者包里有命令行工具、辅助程序在跑 */
const willQuit = (a: AppInfo) => a.running || a.tools_running.length > 0;

function leftoverText(a: AppInfo) {
  if (a.leftovers.length === 0) return t('没有发现残留文件');
  if (a.leftover_bytes == null) return t('{0} 处残留文件', a.leftovers.length);
  return t('{0} 处残留文件，{1}{2}', a.leftovers.length, a.leftover_complete ? '' : t('至少 '), bytes(a.leftover_bytes));
}

/** `active` 为 true 时才第一次加载：列出应用要查一遍系统，不必在用户没点进来时做。
 *  `focus`：从空间去向点「去卸载」过来的应用路径，把它的名字填进搜索框（不替用户勾选）。 */
export function AppsPage({ active, focus }: { active: boolean; focus?: { path: string } | null }) {
  const [apps, setApps] = useState<AppInfo[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [query, setQuery] = useState('');
  const [filter, setFilter] = useState<Filter>('all');
  const [asked, setAsked] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  // 卸载进度：正在卸哪一个、是不是在等它退出
  const [step, setStep] = useState<UninstallProgress | null>(null);
  useEffect(() => api.onUninstallProgress(setStep), []);
  const [result, setResult] = useState<RemoveResult | null>(null);
  // 已删除应用的残留：null 表示还没找过。默认一项都不勾（规格 D-10）
  const [orphans, setOrphans] = useState<Leftover[] | null>(null);
  const [finding, setFinding] = useState(false);
  const [orphanPicked, setOrphanPicked] = useState<Set<string>>(new Set());
  const [orphanConfirming, setOrphanConfirming] = useState(false);
  const ai = useAiAvailable();
  // 有新版本的应用：null 表示还没查过
  const [updates, setUpdates] = useState<AppUpdate[] | null>(null);
  const [checking, setChecking] = useState(false);
  const checkUpdates = () => {
    setChecking(true);
    api
      .appsUpdates()
      .then(setUpdates, (e) => toast(String(e)))
      .finally(() => setChecking(false));
  };

  const lastLoad = useRef(0);
  const load = () => {
    lastLoad.current = Date.now();
    setApps(null);
    setPicked(new Set());
    api.appsList().then(setApps, (e) => {
      toast(t('读取应用列表失败：{0}', e));
      setApps([]);
    });
  };
  const [loaded, setLoaded] = useState(false);
  useEffect(() => {
    if (active && !loaded) {
      setLoaded(true);
      load();
    }
  }, [active, loaded]);

  // 静默重读：列表不清空，搜索词和勾选都留着（已经不在的应用去掉勾选），量过的残留大小也留着。
  // 卸载进行中、确认框开着时不读，免得和卸载的结果打架
  const [refreshing, setRefreshing] = useState(false);
  const refresh = () => {
    if (apps == null || busy || confirming || refreshing) return;
    lastLoad.current = Date.now();
    setRefreshing(true);
    api
      .appsList()
      .then(
        (list) => {
          setApps((old) => {
            const measured = new Map((old ?? []).filter((o) => o.leftover_bytes != null).map((o) => [o.id, o]));
            return list.map((a) => {
              const m = measured.get(a.id);
              return m ? { ...a, leftover_bytes: m.leftover_bytes, leftover_complete: m.leftover_complete } : a;
            });
          });
          setPicked((p) => new Set([...p].filter((id) => list.some((a) => a.id === id))));
        },
        (e) => toast(t('读取应用列表失败：{0}', e)),
      )
      .finally(() => setRefreshing(false));
  };
  // 应用常驻菜单栏，这一页可能停留好几天：再次切到这一页、或窗口重新回到前台时，距上次读超过 30 秒就静默重读
  const refreshRef = useRef(refresh);
  refreshRef.current = refresh;
  useEffect(() => {
    if (!active) return;
    const stale = () => Date.now() - lastLoad.current > 30_000 && refreshRef.current();
    stale();
    window.addEventListener('focus', stale);
    return () => window.removeEventListener('focus', stale);
  }, [active]);
  // 每次点「去卸载」只填一次，之后搜索框归用户
  const focused = useRef<object | null>(null);
  useEffect(() => {
    if (!focus || focus === focused.current || !apps) return;
    focused.current = focus;
    setQuery(apps.find((a) => a.path === focus.path)?.name ?? baseName(focus.path).replace(/\.app$/, ''));
    setFilter('all');
  }, [focus, apps]);

  function toggle(app: AppInfo) {
    const next = new Set(picked);
    if (next.delete(app.id)) return setPicked(next);
    next.add(app.id);
    setPicked(next);
    // 选中时才去量它的残留文件有多大
    if (app.leftover_bytes == null && app.leftovers.length > 0) {
      api.appsMeasure(app.id).then((m) => setApps((list) => list && list.map((a) => (a.id === m.id ? m : a))));
    }
  }

  async function uninstall() {
    setConfirming(false);
    setBusy(true);
    try {
      setResult(await api.appsUninstall([...picked], false));
    } catch (e) {
      toast(t('卸载失败：{0}', String(e)));
    } finally {
      setBusy(false);
      setStep(null);
      load();
    }
  }

  function findOrphans() {
    setFinding(true);
    setOrphanPicked(new Set());
    api
      .appsOrphans()
      .then(setOrphans, (e) => toast(t('查找失败：{0}', String(e))))
      .finally(() => setFinding(false));
  }

  function toggleOrphan(path: string) {
    const next = new Set(orphanPicked);
    if (!next.delete(path)) next.add(path);
    setOrphanPicked(next);
  }

  async function trashOrphans() {
    setOrphanConfirming(false);
    setBusy(true);
    try {
      const r = await api.appsTrashOrphans([...orphanPicked]);
      toast(r.failed.length ? t('已移到废纸篓 {0}，{1} 项没能移走：{2}', bytes(r.trashed_bytes), r.failed.length, r.failed[0].reason) : t('已移到废纸篓 {0}', bytes(r.trashed_bytes)));
    } catch (e) {
      toast(t('清理失败：{0}', String(e)));
    } finally {
      setBusy(false);
      findOrphans();
    }
  }

  const q = query.trim().toLowerCase();
  const shown = (apps ?? []).filter(
    (a) => (filter === 'all' || filter === 'new' || (filter === 'stale' && isStale(a)) || (filter === 'big' && a.size_bytes >= BIG_BYTES)) && a.name.toLowerCase().includes(q),
  );
  // 「最近安装」按应用包创建时间倒序；其他情况沿用后端的按大小排
  if (filter === 'new') shown.sort((a, b) => (b.created_ms ?? 0) - (a.created_ms ?? 0));
  const chosen = (apps ?? []).filter((a) => picked.has(a.id));
  const chosenBytes = chosen.reduce((s, a) => s + total(a), 0);

  return (
    <>
      <div className="scroll hasbar">
        <h1>{t('卸载软件')}</h1>
        <p>{t('连同缓存、偏好设置等残留文件一起移到废纸篓')}</p>
        <div className="tools">
          <input className="search" type="search" placeholder={t('搜索应用')} aria-label={t('搜索应用')} value={query} onChange={(e) => setQuery(e.target.value)} />
          <div className="seg">
            {(['all', 'new', 'stale', 'big'] as const).map((f) => (
              <button key={f} className={filter === f ? 'on' : ''} onClick={() => setFilter(f)}>
                {{ all: t('全部'), new: t('最近安装'), stale: t('不常用'), big: t('大型应用') }[f]}
              </button>
            ))}
          </div>
          <button className="btn gray sm" disabled={apps == null || busy || refreshing} onClick={refresh}>
            {refreshing ? t('正在刷新…') : t('刷新')}
          </button>
        </div>
        <div className="group" data-empty={apps == null ? t('正在读取应用列表…') : t('没有符合条件的应用')}>
          {shown.map((a) => (
            <Fragment key={a.id}>
              <label
                className="row"
                onContextMenu={(e) => {
                  e.preventDefault();
                  revealMenu(() => api.appReveal(a.id).catch((err) => toast(String(err))));
                }}
              >
                <span className={`ic lg ${tint(a.name)}`} aria-hidden="true">
                  {a.name.charAt(0).toUpperCase()}
                </span>
                <span className="grow">
                  <span>
                    {a.name}
                    {a.running && <em className="tag">{t('正在运行')}</em>}
                    {!a.running && isStale(a) && <em className="tag">{t('不常用')}</em>}
                  </span>
                  <small>
                    {a.version && t('版本 {0} · ', a.version)}
                    {a.running ? t('正在使用') : t('上次使用：{0}', ago(a.last_used_ms))} · {leftoverText(a)}
                  </small>
                </span>
                <span className="num">{bytes(a.size_bytes)}</span>
                {ai && (
                  <button className="aib" onClick={() => setAsked(asked === a.id ? null : a.id)} aria-label={t('让 AI 说明卸载 {0} 前要注意什么', a.name)} aria-expanded={asked === a.id}>
                    <Icon name="scan" />
                  </button>
                )}
                <input className="ck" type="checkbox" checked={picked.has(a.id)} onChange={() => toggle(a)} />
              </label>
              {asked === a.id && (
                <div className="ai-in">
                  <AiAnswer ask={(onDelta, fresh) => api.aiExplainUninstall(a.id, onDelta, fresh)} caution={t('卸载前请自行确认')} />
                </div>
              )}
            </Fragment>
          ))}
        </div>

        <div className="gh">{t('软件更新')}</div>
        <div className="group plain">
          {updates == null || checking ? (
            <button className="row" onClick={checkUpdates} disabled={checking || apps == null}>
              <span className="grow">
                <span>{checking ? t('正在检查…') : t('检查哪些应用有新版本')}</span>
                <small>{t('只查自带自动更新（Sparkle）的应用，会连接各应用自己的更新地址。App Store 的应用请在 App Store 里更新。')}</small>
              </span>
              <Icon name="chev" className="chev" />
            </button>
          ) : updates.length === 0 ? (
            <div className="row">
              <span className="grow">{t('都是最新版本')}</span>
            </div>
          ) : (
            updates.map((u) => (
              <div className="row" key={u.id}>
                <span className="grow">
                  <span>{u.name}</span>
                  <small>{t('{0} → {1}', u.current, u.latest)}</small>
                </span>
                <button className="btn gray sm" onClick={() => api.appOpen(u.id).catch((e) => toast(String(e)))}>
                  {t('打开更新')}
                </button>
              </div>
            ))
          )}
        </div>

        <div className="gh">{t('已删除应用的残留')}</div>
        <div className="group plain">
          {orphans == null ? (
            <button className="row" onClick={findOrphans} disabled={finding}>
              <span className="grow">
                <span>{finding ? t('正在查找…') : t('查找已删除应用的残留')}</span>
                <small>{t('只找应用容器和窗口状态，同一开发者还有应用装着的不算，宁可漏掉也不认错。默认不勾选，只会移到废纸篓。')}</small>
              </span>
              <Icon name="chev" className="chev" />
            </button>
          ) : orphans.length === 0 ? (
            <div className="row">
              <span className="grow">{t('没有发现已删除应用的残留')}</span>
            </div>
          ) : (
            <>
              {orphans.map((o) => (
                <label className="row" key={o.path}>
                  <span className="grow">
                    <span>
                      {baseName(o.path)}
                      <em className="tag">{t('需确认')}</em>
                    </span>
                    <small>{shortPath(o.path)}</small>
                  </span>
                  <span className="num">{bytes(o.size_bytes ?? 0)}</span>
                  <input className="ck" type="checkbox" checked={orphanPicked.has(o.path)} onChange={() => toggleOrphan(o.path)} />
                </label>
              ))}
              <div className="row">
                <span className="grow" />
                <button className="btn sm red" disabled={orphanPicked.size === 0 || busy} onClick={() => setOrphanConfirming(true)}>
                  {t('移到废纸篓（{0} 项）', orphanPicked.size)}
                </button>
              </div>
            </>
          )}
        </div>
      </div>

      <div className="bar">
        <span>{t('已选 {0} 个应用', chosen.length)}</span>
        <span className="num">{bytes(chosenBytes)}</span>
        <button className="btn red" disabled={chosen.length === 0 || busy} onClick={() => setConfirming(true)}>
          {busy ? (step?.waiting ? t('正在等「{0}」退出…', step.name) : step ? t('正在卸载 {0}/{1}', step.done + 1, step.total) : t('正在卸载…')) : t('卸载')}
        </button>
      </div>

      {confirming && (
        <Dialog
          title={t('卸载 {0} 个应用？', chosen.length)}
          actions={
            <>
              <button className="btn gray" onClick={() => setConfirming(false)}>
                {t('取消')}
              </button>
              <button className="btn red" onClick={uninstall}>
                {t('卸载')}
              </button>
            </>
          }
        >
          <div className="group">
            {chosen.map((a) => (
              <div className="row" key={a.id}>
                <span className="grow">
                  <span>
                    {a.name}
                    {willQuit(a) && <em className="tag">{t('会先退出')}</em>}
                  </span>
                  <small>{leftoverText(a)}</small>
                  {!a.running && a.tools_running.length > 0 && <small>{t('包里的 {0} 正在运行，会先请它们退出', a.tools_running.slice(0, 3).join(t('、')))}</small>}
                  {a.kept.map((k) => (
                    <small key={k.path} title={k.path}>
                      {k.shared_with != null ? t('与 {0} 共用，已保留：{1}', k.shared_with, shortPath(k.path)) : t('需要管理员权限，不会删除：{0}', shortPath(k.path))}
                    </small>
                  ))}
                </span>
                <span className="num">{bytes(total(a))}</span>
              </div>
            ))}
          </div>
          <p>{t('应用和它的残留文件会移到废纸篓。清空废纸篓之前都还能找回来，清空之后才释放空间。')}</p>
          {chosen.some(willQuit) && <p>{t('正在运行的应用会先被请求退出；包里还在运行的命令行工具和辅助程序会收到结束信号，正在跑的任务（比如构建）会被中断。每个应用最多等 10 秒，没有退出就跳过它，不卸载。')}</p>}
          {chosen.some((a) => a.kept.some((k) => k.shared_with == null)) && <p>{t('「需要管理员权限」的是系统级的启动项和助手程序，本应用不删。应用卸载后它们通常会失效；想彻底清掉，可以在访达里自己移到废纸篓。')}</p>}
        </Dialog>
      )}

      {orphanConfirming && (
        <Dialog
          title={t('把 {0} 项移到废纸篓？', orphanPicked.size)}
          actions={
            <>
              <button className="btn gray" onClick={() => setOrphanConfirming(false)}>
                {t('取消')}
              </button>
              <button className="btn red" onClick={trashOrphans}>
                {t('移到废纸篓（{0} 项）', orphanPicked.size)}
              </button>
            </>
          }
        >
          <p>{t('这些数据对应的应用已经找不到了。如果应用其实还在（比如装在没插上的外接盘里），移走后它会丢失设置和登录状态；清空废纸篓之前都可以放回原处。')}</p>
        </Dialog>
      )}

      {result && (
        <Dialog
          title={t('卸载完成')}
          actions={
            <button className="btn" onClick={() => setResult(null)}>
              {t('好')}
            </button>
          }
        >
          <div className="group">
            <div className="row">
              <span className="grow">
                <span>{t('已移到废纸篓')}</span>
                <small>{t('清空废纸篓后才释放')}</small>
              </span>
              <span className="num">{bytes(result.trashed_bytes)}</span>
            </div>
            {result.failed.length > 0 && (
              <div className="row">
                <span className="grow">
                  <span>{t('没能卸载')}</span>
                  <small style={{ whiteSpace: 'pre-line', maxHeight: 160, overflowY: 'auto' }}>{result.failed.map((f) => t('{0}：{1}', baseName(f.path), f.reason)).join('\n')}</small>
                </span>
                <span className="num">{t('{0} 项', result.failed.length)}</span>
              </div>
            )}
          </div>
        </Dialog>
      )}
    </>
  );
}
