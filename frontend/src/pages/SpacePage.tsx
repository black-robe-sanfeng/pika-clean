import { type CSSProperties, Fragment, useEffect, useMemo, useRef, useState } from 'react';
import { AiAnswer } from '../components/AiAnswer';
import { Dialog } from '../components/Dialog';
import { Icon } from '../components/Icon';
import { toast } from '../components/Toast';
import { api, revealMenu, type SpaceNode, type SpaceProgress, type SpaceReport, useAiAvailable, useDisk } from '../api';
import { bytes, pct } from '../lib/fmt';
import { t } from '../lib/i18n.ts';
import { type Rect, squarify } from '../lib/treemap.ts';

// 树图坐标系宽 300 高 100，和 .tmap 的 aspect-ratio 一致，算完直接换成百分比；LABEL 是名字那一行大约占的高度
const TW = 300;
const TH = 100;
const LABEL = 8;
const at = (r: Rect, w: number, h: number): CSSProperties => ({ left: `${(r.x / w) * 100}%`, top: `${(r.y / h) * 100}%`, width: `${(r.w / w) * 100}%`, height: `${(r.h / h) * 100}%` });
// 一层里的各块，「其余较小的项」也占一块（n 为空），按大小降序
const pieces = (n: SpaceNode) =>
  [...n.children.map((c) => ({ n: c as SpaceNode | null, size: c.size_bytes })), ...(n.other_bytes > 0 ? [{ n: null, size: n.other_bytes }] : [])].sort((a, b) => b.size - a.size);

// 一行放不下时大小换到第二行，名字过长再省略
const Label = ({ name, size }: { name: string; size?: number }) => (
  <span className="tl">
    <b>{name}</b>
    {size !== undefined && <small>{bytes(size)}</small>}
  </span>
);

// 统计时间写成「10/16 14:32」，各种语言都看得懂
const when = (ms: number) => {
  const d = new Date(ms);
  const p2 = (n: number) => String(n).padStart(2, '0');
  return `${d.getMonth() + 1}/${d.getDate()} ${p2(d.getHours())}:${p2(d.getMinutes())}`;
};
// 个人文件夹写成 ~，位置名短一些
const short = (path: string) => path.replace(/^\/Users\/[^/]+/, '~');

// 应用目录里的应用，在卸载页连同残留一起删
const isApp = (path: string) => /^\/(Users\/[^/]+\/)?Applications\/[^/]+\.app$/.test(path);
const find = (n: SpaceNode, path: string): SpaceNode | undefined => {
  if (n.path === path) return n;
  for (const c of n.children) {
    const f = find(c, path);
    if (f) return f;
  }
};

/** 当前这层的矩形树图，面积就是大小，大块里再画出下一层。只是总览，键盘和读屏走下面的列表 */
function TreeMap({ node, onOpen }: { node: SpaceNode; onOpen: (path: SpaceNode[]) => void }) {
  // 排序和铺块只随这一层变，统计进度、快照刷新时不重算
  const layout = useMemo(() => {
    const ps = pieces(node);
    const rs = squarify(
      ps.map((p) => p.size),
      { x: 0, y: 0, w: TW, h: TH },
    );
    return ps.map((p, i) => {
      const r = rs[i];
      const kids = p.n && r.w >= 40 && r.h >= 30 && p.n.children.length > 0 ? pieces(p.n) : [];
      const box = { x: 0, y: 0, w: r.w, h: r.h - LABEL };
      return { ...p, r, kids, box, inner: kids.length > 0 ? squarify(kids.map((k) => k.size), box) : [] };
    });
  }, [node]);
  return (
    <div className="tmap" aria-hidden>
      {layout.map(({ n, size, r, kids, box, inner }) => {
        const label = r.w >= 14 && r.h >= 9;
        if (!n)
          return (
            <div key="" className="tm off" style={at(r, TW, TH)} title={`${t('其余较小的项')} · ${bytes(size)}`}>
              {label && <Label name={t('其余较小的项')} size={size} />}
            </div>
          );
        return (
          <div
            key={n.path}
            // 画了下一层的块是个外框，名字做成深色标题栏，免得看成一个小块
            className={kids.length > 0 ? 'tm grp' : 'tm'}
            style={at(r, TW, TH)}
            title={`${n.name} · ${bytes(size)}`}
            onClick={() => n.children.length > 0 && onOpen([n])}
          >
            {/* 外框标题栏只写名字：总数写上去会拿去和里面的块比面积，大小看下面列表或悬停 */}
            {label && <Label name={n.name} size={kids.length > 0 ? undefined : size} />}
            {kids.length > 0 && (
              <div className="tm-in">
                {kids.map(({ n: g, size: gs }, j) => (
                  <div
                    key={g?.path ?? ''}
                    className={g ? 'tm' : 'tm off'}
                    style={at(inner[j], box.w, box.h)}
                    title={`${g?.name ?? t('其余较小的项')} · ${bytes(gs)}`}
                    onClick={(e) => {
                      if (!g) return;
                      e.stopPropagation();
                      onOpen(g.children.length > 0 ? [n, g] : [n]);
                    }}
                  >
                    {inner[j].w >= 14 && inner[j].h >= 7 && <Label name={g?.name ?? t('其余较小的项')} size={gs} />}
                  </div>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** 统计进度环。事先不知道总量，拿 Finder 的「已用」估个进度；数据卷之外的系统部分数不到，所以统计中最多显示 99%。
 *  自己订阅快照，快照刷新时只重画这一块 */
function Ring({ bytes: done }: { bytes: number }) {
  const used = useDisk()?.used_bytes;
  const percent = used ? Math.min(99, Math.round((done / used) * 100)) : 0;
  return (
    <div className="ring" style={{ '--p': percent } as CSSProperties}>
      <i />
      <b>{percent}%</b>
    </div>
  );
}

/** 「系统与其他」= Finder 的已用 − 数据卷统计到的。只在最上层、统计完时由调用方渲染 */
function SystemRow({ counted }: { counted: number }) {
  const used = useDisk()?.used_bytes;
  const system = used ? Math.max(0, used - counted) : 0;
  if (system <= 0) return null;
  return (
    <div className="row">
      <span className="grow">
        <span>{t('系统与其他')}</span>
        <small>{t('系统卷、本地快照等，统计不到具体文件')}</small>
      </span>
      <span className="num">{bytes(system)}</span>
    </div>
  );
}

/** 空间去向：一层层看启动盘的空间被什么占了。个人文件可以移到废纸篓，能清理的跳到智能扫描，应用跳到卸载（规格切片 16、18）。 */
export function SpacePage({ active, onGo }: { active: boolean; onGo: (view: 'scan' | 'apps', path: string) => void }) {
  const [report, setReport] = useState<SpaceReport | null>(null);
  const [progress, setProgress] = useState<SpaceProgress | null>(null);
  const [running, setRunning] = useState(false);
  // 点进去的每一层；空表示在最上层
  const [trail, setTrail] = useState<SpaceNode[]>([]);
  const [asked, setAsked] = useState<string | null>(null);
  // 正在确认要不要移到废纸篓的那一项
  const [trashing, setTrashing] = useState<SpaceNode | null>(null);
  const ai = useAiAvailable();
  // 重新统计前那份结果的统计时间：取消后后端发回的还是它，就告诉用户保留了上次的结果
  const prevAt = useRef<number | null>(null);

  // 第一次切到这一页才读上次的结果，不在启动时读
  const [loaded, setLoaded] = useState(false);
  useEffect(() => {
    if (active && !loaded) {
      setLoaded(true);
      api.spaceLatest().then((r) => r && setReport(r));
    }
  }, [active, loaded]);

  useEffect(() => {
    const offProgress = api.onSpaceProgress(setProgress);
    const offDone = api.onSpaceDone((r) => {
      if (prevAt.current != null && r.scanned_at_ms === prevAt.current) toast(t('已取消，保留上次的统计结果'));
      setReport(r);
      // 统计期间可以接着看旧结果：点进去的每一层换成新结果里的同一个位置，新结果里没有的那层起退回上一层
      setTrail((tr) => {
        const next: SpaceNode[] = [];
        for (const x of tr) {
          const f = find(r.root, x.path);
          if (!f) break;
          next.push(f);
        }
        return next;
      });
      setRunning(false);
      setProgress(null);
    });
    return () => {
      offProgress();
      offDone();
    };
  }, []);

  const start = () => {
    prevAt.current = report?.scanned_at_ms ?? null;
    setRunning(true);
    setProgress(null);
    api.spaceStart();
  };
  const reveal = (path: string) => api.spaceReveal(path).catch((e) => toast(String(e)));
  const trash = (n: SpaceNode) => {
    setTrashing(null);
    api
      .spaceTrash(n.path)
      .then((r) => {
        setReport(r);
        // 后端改了大小，点进去的每一层换成新结果里的同一个节点
        setTrail((tr) => tr.map((x) => find(r.root, x.path) ?? x));
        toast(t('已把「{0}」移到废纸篓，清空废纸篓后才释放空间', n.name));
      })
      .catch((e) => toast(String(e)));
  };
  const open = (n: SpaceNode) => {
    setTrail([...trail, n]);
    setAsked(null);
  };

  const here = trail.length > 0 ? trail[trail.length - 1] : report?.root;
  const atTop = trail.length === 0;

  // 还没有结果时和智能扫描首页一样：中间一个大按钮，第一次统计时显示进度
  if (!report) {
    return (
      <div className="scan" data-state={running ? 'run' : 'idle'}>
        <div className="s-idle">
          <button className="orb" onClick={start}>
            {t('统计')}
          </button>
          <p>{t('一层层看启动盘的空间被哪些文件夹占用，个人文件可以移到废纸篓')}</p>
        </div>
        <div className="s-run">
          <Ring bytes={progress?.bytes ?? 0} />
          <h1>{t('正在统计…')}</h1>
          <p>{t('已看 {0} 个文件 · {1}', (progress?.files ?? 0).toLocaleString(), bytes(progress?.bytes ?? 0))}</p>
          {/* 边数边显示：目前最大的几个位置，数完的打勾 */}
          {progress && progress.tops.length > 0 && (
            <div className="group tops">
              {progress.tops.map((top) => (
                <div className="row" key={top.path}>
                  <span className="grow">{short(top.path)}</span>
                  <i className="tbar" style={{ '--w': `${(top.bytes / progress.tops[0].bytes) * 100}%` } as CSSProperties} />
                  <span className="num">{bytes(top.bytes)}</span>
                  {top.done ? <Icon name="check" className="tick" /> : <span className="spin" />}
                </div>
              ))}
            </div>
          )}
          <button className="btn sm" style={{ marginTop: 10 }} onClick={() => api.spaceCancel()}>
            {t('取消')}
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="scroll">
      <h1 className="title-row">
        {t('空间去向')}
        {/* 统计期间旧结果留着，数完才换成新的；取消的话保留旧结果 */}
        <button className="btn sm" disabled={running} onClick={start}>
          {t('重新统计')}
        </button>
      </h1>
      <p>
        {running
          ? t('正在统计… 已看 {0} 个文件，{1}', (progress?.files ?? 0).toLocaleString(), bytes(progress?.bytes ?? 0))
          : report.finished
            ? t('共 {0}，大小是大致数字', bytes(report.root.size_bytes))
            : t('未统计完，数字偏小')}
        {!running && report.scanned_at_ms > 0 && t(' · 统计于 {0}', when(report.scanned_at_ms))}
      </p>
      {running && (
        <button className="btn sm" onClick={() => api.spaceCancel()}>
          {t('取消')}
        </button>
      )}
      {report && here && (
        <>
          <div className="crumbs">
            <button className="link" onClick={() => setTrail([])}>
              {t('启动盘')}
            </button>
            {trail.map((n, i) => (
              <Fragment key={n.path}>
                <span>›</span>
                <button className="link" onClick={() => setTrail(trail.slice(0, i + 1))}>
                  {n.name}
                </button>
              </Fragment>
            ))}
          </div>
          {here.size_bytes > 0 && <TreeMap node={here} onOpen={(path) => { setTrail([...trail, ...path]); setAsked(null); }} />}
          <div className="group">
            {here.children.map((n) => (
              <Fragment key={n.path}>
                <div
                  className="row"
                  onContextMenu={(e) => {
                    e.preventDefault();
                    revealMenu(() => reveal(n.path));
                  }}
                >
                  <span className="grow">
                    <span>
                      {n.name}
                      {n.cleanable && (
                        <button className="link" style={{ fontSize: 12, marginLeft: 8 }} onClick={() => onGo('scan', n.path)}>
                          {t('去清理')}
                        </button>
                      )}
                      {isApp(n.path) && (
                        <button className="link" style={{ fontSize: 12, marginLeft: 8 }} onClick={() => onGo('apps', n.path)}>
                          {t('去卸载')}
                        </button>
                      )}
                    </span>
                    <div className="meter" style={{ '--p': pct(n.size_bytes, here.size_bytes) } as CSSProperties}>
                      <i />
                    </div>
                  </span>
                  <span className="num">{bytes(n.size_bytes)}</span>
                  {n.trashable && (
                    <button className="aib" style={{ color: 'var(--red)', background: 'var(--fill)' }} disabled={running} onClick={() => setTrashing(n)} aria-label={t('把 {0} 移到废纸篓', n.name)}>
                      <Icon name="trash" />
                    </button>
                  )}
                  {ai && (
                    <button className="aib" onClick={() => setAsked(asked === n.path ? null : n.path)} aria-label={t('让 AI 解释 {0} 是什么', n.name)} aria-expanded={asked === n.path}>
                      <Icon name="scan" />
                    </button>
                  )}
                  <button className="aib" style={{ color: 'var(--sub)', background: 'var(--fill)' }} disabled={n.children.length === 0} onClick={() => open(n)} aria-label={t('打开 {0}', n.name)}>
                    <Icon name="chev" />
                  </button>
                </div>
                {asked === n.path && (
                  <div className="ai-in">
                    <AiAnswer ask={(onDelta, fresh) => api.aiExplainSpace(n.path, onDelta, fresh)} caution={t('删除前请自行确认')} />
                  </div>
                )}
              </Fragment>
            ))}
            {here.other_bytes > 0 && (
              <div className="row off">
                <span className="grow">
                  <span>{t('其余较小的项')}</span>
                  <small>{t('没有单独列出的小文件和文件夹')}</small>
                </span>
                <span className="num">{bytes(here.other_bytes)}</span>
              </div>
            )}
            {/* 没统计完时差值里混着没数到的部分，不显示 */}
            {atTop && report.finished && <SystemRow counted={report.root.size_bytes} />}
          </div>
          {atTop && report.unreadable_count > 0 && (
            <small style={{ display: 'block', margin: '8px 14px 0' }}>
              {t('有 {0} 个文件夹没有权限读取，实际占用比显示的多', report.unreadable_count)}
              <button className="link" style={{ fontSize: 12, marginLeft: 8 }} onClick={() => api.openFullDiskAccessSettings().catch((e) => toast(String(e)))}>
                {t('去授权')}
              </button>
            </small>
          )}
          {atTop && report.purgeable_bytes > 0 && (
            <small style={{ display: 'block', margin: '6px 14px 0' }}>{t('另有 {0} 可清除空间，系统需要时会自动腾出', bytes(report.purgeable_bytes))}</small>
          )}
        </>
      )}
      {trashing && (
        <Dialog
          title={t('把「{0}」移到废纸篓？', trashing.name)}
          actions={
            <>
              <button className="btn gray" onClick={() => setTrashing(null)}>
                {t('取消')}
              </button>
              <button className="btn red" onClick={() => trash(trashing)}>
                {t('放进废纸篓')}
              </button>
            </>
          }
        >
          <p>{t('{0}，{1}。清空废纸篓之前都还能找回来，清空之后才释放空间。', trashing.path, bytes(trashing.size_bytes))}</p>
        </Dialog>
      )}
    </div>
  );
}
