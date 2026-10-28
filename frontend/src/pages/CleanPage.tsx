import { Fragment, useEffect, useRef, useState } from 'react';
import { Check } from '../components/Check';
import { DEV_COMMAND, DevEnvRows, DevEnvSheet, useDevEnv } from '../components/DevEnvGroup';
import { api, revealMenu, type Category, type CategoryId, type CleanProgress, type Item, type LoginItem, type RemoveResult, type ScanReport, type Trashed, useAiAvailable, useDisk, type Organized } from '../api';
import type { ScanState } from '../app/useScan';
import { AiAnswer } from '../components/AiAnswer';
import { AskClean } from '../components/AskClean';
import { Dialog } from '../components/Dialog';
import { Icon, Tile } from '../components/Icon';
import { Sheet } from '../components/Sheet';
import { toast } from '../components/Toast';
import { CATEGORY, cleanable, cleanableBytes, REMOVAL } from '../lib/categories';
import { Insights } from './ScanPage';
import { ago, bytes } from '../lib/fmt';
import { t } from '../lib/i18n.ts';
import { categoryAt, devGroupAt } from '../lib/jump.ts';

/** 明细面板最多列这么多条，其余合并成一行。条目按大小降序，所以大头都在前面。 */
const DETAIL_LIMIT = 100;

const baseName = (path: string) => path.slice(path.lastIndexOf('/') + 1);
// 这两类的条目名字都差不多（node_modules、_cacache），带上上一级目录才分得清是谁的
// AI 工具的条目散在主目录各处，名字前面带上是哪个工具的（按顺序匹配，先认更具体的）
const AI_TOOLS: [string, string][] = [
  ['/.claude/', 'Claude Code'],
  ['/claude-cli', 'Claude Code'],
  ['/Claude/', 'Claude'],
  ['/.codex/', 'Codex'],
  ['opencode', 'OpenCode'],
  ['/.gemini/', 'Gemini CLI'],
  ['/Cursor/', 'Cursor'],
];
const BROWSERS: [string, string][] = [
  ['/Google/Chrome/', 'Chrome'],
  ['/Microsoft Edge/', 'Edge'],
  ['/Brave-Browser/', 'Brave'],
];
const itemName = (cat: CategoryId, path: string) => {
  const tail = (n: number) => path.split('/').slice(-n).join('/');
  if (cat === 'ai_cache' || cat === 'ai_history') {
    const tool = AI_TOOLS.find(([k]) => path.includes(k))?.[1];
    return tool ? `${tool} · ${tail(cat === 'ai_history' ? 2 : 1)}` : tail(2);
  }
  if (cat === 'browser_privacy') return `${BROWSERS.find(([k]) => path.includes(k))?.[1] ?? 'Safari'} · ${baseName(path)}`;
  return cat === 'stale_projects' || cat === 'dev_cache' ? tail(2) : baseName(path);
};

const sizeOf = (items: { size_bytes: number }[]) => items.reduce((s, i) => s + i.size_bytes, 0);

function blocked(c: Category): string | null {
  if (c.unreadable) return t('需要完全磁盘访问');
  return null;
}

function Storage({ cleanBytes }: { cleanBytes: number }) {
  const disk = useDisk();
  if (!disk) return null;
  const free = disk.total_bytes - disk.used_bytes;
  const used = Math.max(0, disk.used_bytes - cleanBytes);
  return (
    <div className="store">
      <div className="top">
        <b>{t('启动磁盘')}</b>
        <span style={{ color: 'var(--sub)' }}>{t('已用 {0} / {1}', bytes(disk.used_bytes), bytes(disk.total_bytes))}</span>
      </div>
      <div className="sbar" aria-hidden="true">
        <i style={{ flex: used, background: 'var(--gray)' }} />
        <i style={{ flex: cleanBytes, background: 'var(--green)' }} />
        <i style={{ flex: free, background: 'var(--fill)' }} />
      </div>
      <div className="legend">
        <span>
          <i style={{ background: 'var(--gray)' }} />
          {t('其他已用')}{' '}{bytes(used)}
        </span>
        <span>
          <i style={{ background: 'var(--green)' }} />
          {t('可清理')}{' '}{bytes(cleanBytes)}
        </span>
        <span>
          <i style={{ background: 'var(--fill)' }} />
          {t('可用')}{' '}{bytes(free)}
        </span>
      </div>
    </div>
  );
}

/** 让 AI 只根据文件名给旧下载文件分组。只展示建议，不勾选也不删除任何东西。 */
function Organize() {
  const [open, setOpen] = useState(false);
  const [groups, setGroups] = useState<Organized['groups']>([]);
  const ADVICE = { delete: t('建议删'), keep: t('建议留'), review: t('自己看看') } as const;
  if (!open) {
    return (
      <button className="pill" style={{ justifySelf: 'start' }} onClick={() => setOpen(true)}>
        <Icon name="scan" />
        {t('让 AI 按文件名分类')}
      </button>
    );
  }
  return (
    <>
      <AiAnswer
        ask={(_, fresh) => api.aiOrganizeDownloads(fresh).then((o) => ({ ...o, text: o.groups.length ? t('分成了 {0} 组，只看了最大的一批文件的名字。', o.groups.length) : t('没能分出有意义的组。') }))}
        onAnswer={(o) => setGroups(o.groups)}
        caution={t('分组只是建议，不会替你勾选')}
      />
      {groups.length > 0 && (
        <div className="group">
          {groups.map((g) => (
            <div className="row" key={g.title}>
              <span className="grow">
                {/* 名字太长时截名字，不截「建议删 / 建议留」；理由可以换行 */}
                <span style={{ display: 'flex', alignItems: 'center' }}>
                  <span style={{ overflow: 'hidden', textOverflow: 'ellipsis' }}>{g.title}</span>
                  <em className="tag" style={{ flex: 'none' }}>
                    {ADVICE[g.advice]}
                  </em>
                </span>
                <small style={{ whiteSpace: 'normal' }}>{t('{0} 个 · {1}', g.item_ids.length, g.why)}</small>
              </span>
              <span className="num">{bytes(g.total_bytes)}</span>
            </div>
          ))}
        </div>
      )}
    </>
  );
}

function Details({
  category,
  picked,
  onPick,
  ignored,
  onIgnore,
  onClose,
}: {
  category: Category | null;
  /** 已勾选的条目编号 */
  picked: Set<string>;
  /** 勾上或去掉一批条目 */
  onPick: (ids: string[], on: boolean) => void;
  /** 「不再提示」的条目路径：照样列出，但「全选」不勾它们 */
  ignored: Set<string>;
  onIgnore: (item: Item, on: boolean) => void;
  onClose: () => void;
}) {
  // 关闭动画期间继续显示上一类的内容
  const shown = useRef<Category | null>(null);
  if (category) shown.current = category;
  const c = shown.current;
  const ai = useAiAvailable();
  // 正在看 AI 解释的那个条目。换类别时收起。
  const [asked, setAsked] = useState<string | null>(null);
  useEffect(() => setAsked(null), [category?.id]);
  const mine = c ? c.items.filter((i) => picked.has(i.id)) : [];
  // 「全选」只勾没被忽略的；忽略的只能逐个手动勾
  const can = c ? c.items.filter((i) => !ignored.has(i.path)) : [];
  const allCan = mine.length > 0 && can.every((i) => picked.has(i.id));
  // 排在后面的小条目不逐条列出，只能整批勾选
  const rest = c ? c.items.slice(DETAIL_LIMIT) : [];
  const restMine = rest.filter((i) => picked.has(i.id)).length;
  const restCan = rest.filter((i) => !ignored.has(i.path));
  return (
    <Sheet
      open={category != null}
      title={c ? CATEGORY[c.id].name : ''}
      onClose={onClose}
      footer={c && (c.removal === 'trash' ? t('清理时移到废纸篓，清空废纸篓后才释放空间。') : t('清理时直接删除，立即释放空间，无法恢复。'))}
    >
      {c && (
        <>
          <p>
            {t('{0} 项 · {1}', c.items.length, (c.partial ? t('至少 ') : '') + bytes(c.total_bytes))}
            {cleanable(c) && (
              <>
                {t('，已选 {0} 项 {1}', mine.length, bytes(sizeOf(mine)))}
                <button className="link" style={{ fontSize: 13, marginLeft: 10 }} onClick={() => (allCan ? onPick(c.items.map((i) => i.id), false) : onPick(can.map((i) => i.id), true))}>
                  {allCan ? t('全不选') : t('全选')}
                </button>
              </>
            )}
          </p>
          {ai && c.id === 'old_downloads' && <Organize key={c.total_bytes} />}
          <div className="group">
            {c.items.slice(0, DETAIL_LIMIT).map((i) => (
              <Fragment key={i.id}>
                <div
                  className="row"
                  title={i.path}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    revealMenu(() => api.itemReveal(i.id).catch((err) => toast(String(err))), [
                      ignored.has(i.path) ? { text: t('恢复提示'), action: () => onIgnore(i, false) } : { text: t('不再提示'), action: () => onIgnore(i, true) },
                    ]);
                  }}
                >
                  <span className="grow">
                    <span>
                      {itemName(c.id, i.path)}
                      {ignored.has(i.path) && <em className="tag">{t('已忽略')}</em>}
                    </span>
                    <small>{t('{0}：{1}', c.id === 'stale_projects' ? t('项目上次改动') : t('上次使用'), ago(i.last_used_ms))}</small>
                  </span>
                  <span className="num">{`${i.partial ? '≥ ' : ''}${bytes(i.size_bytes)}`}</span>
                  {ai && (
                    <button className="aib" onClick={() => setAsked(asked === i.id ? null : i.id)} aria-label={t('让 AI 解释 {0} 是什么', baseName(i.path))} aria-expanded={asked === i.id}>
                      <Icon name="scan" />
                    </button>
                  )}
                  <Check all={picked.has(i.id)} disabled={!cleanable(c)} onChange={() => onPick([i.id], !picked.has(i.id))} aria-label={t('清理 {0}', baseName(i.path))} />
                </div>
                {asked === i.id && (
                  <div className="ai-in">
                    <AiAnswer ask={(onDelta, fresh) => api.aiExplainItem(i.id, onDelta, fresh)} />
                  </div>
                )}
              </Fragment>
            ))}
            {c.items.length > DETAIL_LIMIT && (
              <div className="row">
                <span className="grow">
                  <small>{t('其余 {0} 项', c.items.length - DETAIL_LIMIT)}</small>
                </span>
                <span className="num">{bytes(sizeOf(rest))}</span>
                <Check
                  all={restMine > 0 && restCan.every((i) => picked.has(i.id))}
                  some={restMine > 0}
                  disabled={!cleanable(c)}
                  onChange={() => (restCan.some((i) => !picked.has(i.id)) ? onPick(restCan.map((i) => i.id), true) : onPick(rest.map((i) => i.id), false))}
                  aria-label={t('清理其余 {0} 项', rest.length)}
                />
              </div>
            )}
          </div>
        </>
      )}
    </Sheet>
  );
}

/** 最近一次清理移到废纸篓的文件：逐个放回原处；右键可在访达里看它在废纸篓的哪里。 */
function TrashedSheet({ open, list, onChange, onClose }: { open: boolean; list: Trashed[]; onChange: (list: Trashed[]) => void; onClose: () => void }) {
  const putBack = (r: Trashed) =>
    api.trashPutBack(r.to).then(
      () => {
        toast(t('已放回原处'));
        onChange(list.filter((x) => x !== r));
      },
      (err) => toast(t('没能放回：{0}', String(err))),
    );
  return (
    <Sheet open={open} title={t('最近一次移到废纸篓的文件')} onClose={onClose} footer={t('只记最近一次清理移走的；清空废纸篓后就找不回了。')}>
      <p>{t('{0} 项 · {1}', list.length, bytes(sizeOf(list)))}</p>
      <div className="group">
        {list.map((r) => (
          <div
            className="row"
            key={r.to}
            title={r.from}
            onContextMenu={(e) => {
              e.preventDefault();
              revealMenu(() => api.trashReveal(r.to).catch((err) => toast(String(err))));
            }}
          >
            <span className="grow">
              <span>{baseName(r.from)}</span>
              <small>{r.from.slice(0, r.from.lastIndexOf('/'))}</small>
            </span>
            <span className="num">{bytes(r.size_bytes)}</span>
            <button className="btn gray sm" onClick={() => putBack(r)}>
              {t('放回原处')}
            </button>
          </div>
        ))}
      </div>
    </Sheet>
  );
}

/**
 * 智能扫描的结果：可清理的类别列表、逐个挑文件、确认后清理。重新扫描期间列表留着不闪。
 */
/** onRescan：用户点「返回」，回到首页的大按钮。清理后不整份重扫，后端会发来改好的报告 */
/** focus：从空间去向点「去清理」过来的位置，打开它所在的类别或开发环境那一组 */
export function CleanPage({ scan, onRescan, focus }: { scan: ScanState; onRescan: () => void; focus?: { path: string } | null }) {
  // 重新扫描期间沿用上一份报告，列表不闪
  const last = useRef<ScanReport | null>(null);
  if (scan.phase === 'done') last.current = scan.report;
  const report = last.current;

  // 「不再提示」的条目路径：照样列出，但默认勾选、AI 方案和整类勾选都跳过它们
  const [ignored, setIgnored] = useState<Set<string>>(new Set());
  const can = (c: Category) => c.items.filter((i) => !ignored.has(i.path));
  // 勾选细到条目：存的是条目编号。勾类别等于勾上它的全部（没被忽略的）条目
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const pickCategories = (keep: (c: Category) => boolean) =>
    setPicked(new Set((last.current?.categories ?? []).filter(keep).flatMap((c) => can(c).map((i) => i.id))));
  const [detail, setDetail] = useState<CategoryId | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<RemoveResult | null>(null);
  // 清理进度：正在做哪一步、做到第几项；stopping 表示用户点了「停止」
  const [stage, setStage] = useState<'files' | 'dev' | null>(null);
  const [progress, setProgress] = useState<CleanProgress | null>(null);
  const stopping = useRef(false);
  const [halting, setHalting] = useState(false);
  // 最近一次清理移到废纸篓的文件，可以放回原处
  const [trashed, setTrashed] = useState<Trashed[]>([]);
  const [trashedOpen, setTrashedOpen] = useState(false);
  // 程序已经不存在、还开着的启动代理（规格 O-04 的「建议关闭」）。清理时顺带停用，停用随时能在优化页恢复
  const [stale, setStale] = useState<LoginItem[]>([]);
  const [offStale, setOffStale] = useState(true);
  const ai = useAiAvailable();
  // 开发环境（Docker、模拟器）用它们自己的命令清理，但和上面的类别一起算进底部的总数、一起清。
  // 只在完整扫描后重新读；清理后只改了报告的那几次不读
  const devStamp = useRef<ScanReport | null>(null);
  if (scan.phase === 'done' && !scan.update) devStamp.current = scan.report;
  const dev = useDevEnv(devStamp.current);

  useEffect(() => {
    api.trashLast().then(setTrashed);
    api.loginItemsStale().then(setStale, () => setStale([]));
    return api.onCleanProgress(setProgress);
  }, []);

  // 每来一份新扫描的报告，勾选状态回到后端给的默认值，跳过「不再提示」的（名单可能刚在设置里改过，所以每次重读）；
  // 清理后改过的报告不重置
  useEffect(() => {
    if (scan.phase !== 'done' || scan.update) return;
    api.ignoredList().then((list) => {
      const set = new Set(list);
      setIgnored(set);
      setPicked(new Set(scan.report.categories.filter((c) => cleanable(c) && c.default_selected).flatMap((c) => c.items).filter((i) => !set.has(i.path)).map((i) => i.id)));
    });
  }, [scan]);

  // 每次点「去清理」只打开一次明细，之后由用户自己操作；开发环境还在读时等它读完
  const focused = useRef<object | null>(null);
  useEffect(() => {
    if (!focus || focus === focused.current || !report) return;
    const group = devGroupAt(focus.path);
    if (group && !dev.rows) return;
    focused.current = focus;
    if (group) {
      if ((group === 'docker' ? dev.rows!.dockerRows : [...dev.rows!.deviceRows, ...dev.rows!.runtimeRows, ...(dev.rows!.unavailableRow ? [dev.rows!.unavailableRow] : [])]).length === 0) return;
      setDetail(null);
      dev.setOpen(group);
      return;
    }
    const id = categoryAt(report.categories, focus.path);
    if (id) {
      dev.setOpen(null);
      setDetail(id);
    }
  }, [focus, report, dev.report]);

  const ignore = (i: Item, on: boolean) =>
    (on ? api.ignoreAdd(i.id) : api.ignoreRemove(i.path)).then((list) => {
      setIgnored(new Set(list));
      if (on) pick([i.id], false);
    }, (e) => toast(String(e)));

  if (!report) return null;

  const cats = report.categories;
  const scanning = scan.phase === 'run';
  const total = cleanableBytes(cats);
  // 默认勾选的那部分有多大（个人文件不在其中）
  const defaultBytes = cats.filter((c) => cleanable(c) && c.default_selected).reduce((n, c) => n + sizeOf(can(c)), 0);
  // 每个类别里勾了哪些条目；一条都没勾的类别不算
  const chosen = cats.map((c) => ({ c, items: c.items.filter((i) => picked.has(i.id)) })).filter((x) => x.items.length > 0);
  const devBytes = dev.chosen.reduce((s, r) => s + r.bytes, 0);
  const chosenBytes = chosen.reduce((s, x) => s + sizeOf(x.items), 0) + devBytes;
  const chosenCount = chosen.length + dev.groups;
  const pick = (ids: string[], on: boolean) =>
    setPicked((prev) => {
      const next = new Set(prev);
      for (const id of ids) on ? next.add(id) : next.delete(id);
      return next;
    });

  async function clean() {
    // 清理中、扫描中都不再发起：报告马上要变，不能拿旧报告再删一次
    if (busy || scanning) return;
    setConfirming(false);
    setBusy(true);
    stopping.current = false;
    setHalting(false);
    // 文件和开发环境两步各管各的：一步出错不耽误另一步，最后合成一个结果
    const done: RemoveResult[] = [];
    const errors: string[] = [];
    const step = async (s: 'files' | 'dev', run: () => Promise<RemoveResult>) => {
      if (stopping.current) return;
      setStage(s);
      setProgress(null);
      try {
        done.push(await run());
      } catch (e) {
        errors.push(String(e));
      }
    };
    if (chosen.length) await step('files', () => api.removeItems(chosen.flatMap((x) => x.items.map((i) => i.id)), false));
    if (dev.chosen.length) {
      await step('dev', () => api.devenvClean(dev.chosen.map((r) => r.target), false));
      dev.reload();
    }
    // 停用失效启动项不算进释放的空间；失败的只提示一句，不影响前面的结果
    if (offStale && stale.length && !stopping.current) {
      const off = await Promise.allSettled(stale.map((i) => api.loginItemSet(i, false)));
      const ok = off.filter((r) => r.status === 'fulfilled').length;
      if (ok < stale.length) errors.push(t('有 {0} 个失效的启动项没能停用', stale.length - ok));
      api.loginItemsStale().then(setStale, () => setStale([]));
    }
    setBusy(false);
    setStage(null);
    setProgress(null);
    if (errors.length) toast(t('清理失败：{0}', errors.join('；')));
    if (done.length) {
      const all: RemoveResult = {
        estimated: done.some((r) => r.estimated),
        freed_bytes: done.reduce((s, r) => s + r.freed_bytes, 0),
        trashed_bytes: done.reduce((s, r) => s + r.trashed_bytes, 0),
        failed: done.flatMap((r) => r.failed),
        trashed: done.flatMap((r) => r.trashed),
        warning: done.map((r) => r.warning).filter(Boolean).join(' ') || null,
      };
      if (all.trashed.length) setTrashed(all.trashed);
      // 点「清理」前已经确认过一次：全部顺利时只在底部提示一句，有失败或警告才弹结果框说明原因
      if (all.failed.length || all.warning) {
        setResult(all);
      } else {
        const freed = all.estimated ? t('约 {0}', bytes(all.freed_bytes)) : bytes(all.freed_bytes);
        toast(all.trashed_bytes > 0 ? t('清理完成，已释放 {0}，另有 {1} 移到了废纸篓', freed, bytes(all.trashed_bytes)) : t('清理完成，已释放 {0}', freed));
      }
    }
    // 不整份重扫：后端已经把删掉的条目从报告里去掉，有东西进了废纸篓时只重扫「废纸篓」这一类
  }

  const stop = () => {
    stopping.current = true;
    setHalting(true);
    api.cleanCancel();
  };
  const progressText = progress?.admin
    ? t('等你在系统密码框里输入密码…')
    : stage === 'dev'
      ? progress
        ? t('正在清理开发环境 {0}/{1}', progress.done, progress.total)
        : t('正在清理开发环境…')
      : progress
        ? t('正在清理 {0}/{1} 项', progress.done, progress.total)
        : t('正在清理…');
  const openTrashed = () => {
    setResult(null);
    setDetail(null);
    dev.setOpen(null);
    setTrashedOpen(true);
  };

  return (
    <>
      <div className="scroll hasbar">
        <h1 className="title-row">
          {scanning ? t('正在扫描…') : t('扫描完成')}
          <button className="btn sm" disabled={scanning || busy} onClick={onRescan}>
            {t('返回')}
          </button>
        </h1>
        <p>
          {scan.phase === 'run'
            ? scan.progress
              ? t('正在检查{0}', CATEGORY[scan.progress.category].name)
              : t('准备中')
            : total === 0
              ? t('没有发现可清理的内容')
              : t('发现 {0} 可清理空间{1}', bytes(total), defaultBytes < total ? t('，其中 {0} 已默认勾选', bytes(defaultBytes)) : '')}
          {!scanning && report?.at_ms ? ` · ${t('扫描于 {0}', new Date(report.at_ms).toLocaleString(undefined, { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' }))}` : ''}
        </p>
        <Insights scan={scan} summary={!scanning && total > 0} />
        <Storage cleanBytes={total} />
        {ai && (
          <AskClean
            categories={cats}
            disabled={busy || scanning}
            onPlan={(ids) => pickCategories((c) => ids.includes(c.id))}
            onApply={(ids) => {
              pickCategories((c) => ids.includes(c.id));
              setConfirming(true);
            }}
          />
        )}
        <div className="gh">{t('可清理的文件')}</div>
        <div className="group">
          {cats.map((c) => {
            const meta = CATEGORY[c.id];
            const why = blocked(c);
            const n = c.items.filter((i) => picked.has(i.id)).length;
            // 整类勾选只管没被忽略的条目
            const k = can(c);
            const all = n > 0 && k.every((i) => picked.has(i.id));
            return (
              <label className={`row${!why && c.total_bytes === 0 ? ' off' : ''}`} key={c.id}>
                <Tile name={meta.icon} color={meta.color} />
                <span className="grow">
                  <span>
                    {meta.name}
                    {why && <em className="tag">{why}</em>}
                    {c.unreadable && (
                      <button className="link" style={{ fontSize: 12, marginLeft: 8 }} onClick={() => api.openFullDiskAccessSettings().catch((e) => toast(String(e)))}>
                        {t('去授权')}
                      </button>
                    )}
                    {!why && c.needs_admin && c.total_bytes > 0 && <em className="tag">{t('要输管理员密码')}</em>}
                    {!why && !c.needs_admin && !c.default_selected && c.total_bytes > 0 && <em className="tag">{t('需确认')}</em>}
                    {n > 0 && n < c.items.length && <em className="tag">{t('只选了 {0} 项', n)}</em>}
                  </span>
                  <small>{meta.desc}</small>
                </span>
                <span className="num">{c.unreadable ? '—' : `${c.partial ? '≥ ' : ''}${bytes(c.total_bytes)}`}</span>
                <button className="aib" style={{ color: 'var(--sub)', background: 'var(--fill)' }} disabled={c.items.length === 0} onClick={() => {
                    dev.setOpen(null);
                    setTrashedOpen(false);
                    setDetail(c.id);
                  }} aria-label={t('查看{0}的明细', meta.name)}>
                  <Icon name="chev" />
                </button>
                <Check all={all} some={n > 0} disabled={!cleanable(c)} onChange={() => (all ? pick(c.items.map((i) => i.id), false) : pick(k.map((i) => i.id), true))} />
              </label>
            );
          })}
          <DevEnvRows
            dev={dev}
            onOpen={() => {
              setDetail(null);
              setTrashedOpen(false);
            }}
          />
        </div>
        <small style={{ display: 'block', margin: '8px 14px 0' }}>{t('勾选要清理的类别，点右边的箭头可以逐个挑文件；个人文件不会被默认选中')}</small>
        <small style={{ display: 'block', margin: '4px 14px 0' }}>{t('在明细里右键一个条目可以「不再提示」，以后不再默认勾选它')}</small>
        {trashed.length > 0 && (
          <button className="link" style={{ display: 'block', margin: '8px 14px 0', fontSize: 13 }} onClick={openTrashed}>
            {t('最近一次移到废纸篓的 {0} 项，可以放回原处', trashed.length)}
          </button>
        )}
      </div>

      <div className="bar">
        <span>{busy ? progressText : t('已选 {0} 类', chosenCount)}</span>
        <span className="num">{bytes(chosenBytes)}</span>
        {busy ? (
          <button className="btn gray" disabled={halting || progress?.admin} onClick={stop}>
            {halting ? t('正在停止…') : t('停止')}
          </button>
        ) : (
          <button className="btn" disabled={chosenCount === 0 || scanning} onClick={() => setConfirming(true)}>
            {scanning ? t('正在扫描…') : t('清理')}
          </button>
        )}
      </div>

      <Details category={cats.find((c) => c.id === detail) ?? null} picked={picked} onPick={pick} ignored={ignored} onIgnore={ignore} onClose={() => setDetail(null)} />
      <DevEnvSheet dev={dev} />
      <TrashedSheet open={trashedOpen} list={trashed} onChange={setTrashed} onClose={() => setTrashedOpen(false)} />

      {confirming && chosenCount > 0 && !busy && !scanning && (
        <Dialog
          title={t('清理 {0}？', bytes(chosenBytes))}
          actions={
            <>
              <button className="btn gray" onClick={() => setConfirming(false)}>
                {t('取消')}
              </button>
              <button className="btn red" onClick={clean}>
                {t('清理')}
              </button>
            </>
          }
        >
          <div className="group">
            {chosen.map(({ c, items }) => (
              <div className="row" key={c.id}>
                <span className="grow">
                  <span>{CATEGORY[c.id].name}</span>
                  <small>
                    {REMOVAL[c.removal]}
                    {items.length < c.items.length && t(' · 只清选中的 {0} 项', items.length)}
                  </small>
                </span>
                <span className="num">{bytes(sizeOf(items))}</span>
              </div>
            ))}
            {stale.length > 0 && (
              <label className="row">
                <span className="grow">
                  <span>{t('停用 {0} 个失效的启动项', stale.length)}</span>
                  <small>{t('它们要运行的程序已经不存在，每次登录都会启动失败。可以在「优化速度」里重新启用')}</small>
                </span>
                <input type="checkbox" checked={offStale} onChange={(e) => setOffStale(e.target.checked)} />
              </label>
            )}
            {dev.chosen.map((r) => (
              <div className="row" key={r.key}>
                <span className="grow">
                  <span>{r.name}</span>
                  <small>
                    <code>
                      {DEV_COMMAND[r.target.kind]}
                      {'id' in r.target ? ` ${r.target.id}` : ''}
                    </code>
                  </small>
                </span>
                <span className="num">{bytes(r.bytes)}</span>
              </div>
            ))}
          </div>
          <p>
            {chosen.some((x) => x.c.removal === 'permanent') && t('直接删除的内容无法恢复。')}
            {chosen.some((x) => x.c.removal === 'trash') && t('移到废纸篓的内容要清空废纸篓后才释放空间。')}
            {chosen.some((x) => x.c.needs_admin) && t('系统缓存和崩溃转储需要管理员权限，点「清理」后系统会弹出密码框，密码由系统处理。')}
            {chosen.some((x) => x.c.id === 'ai_cache') && t('AI 工具开着时清理它的缓存，可能打断它正在做的事，建议先退出。')}
            {chosen.some((x) => x.c.id === 'browser_privacy') && t('清理浏览器记录前先退出对应的浏览器，清理后各网站要重新登录。')}
            {dev.chosen.length > 0 && t('用 Docker 和 Xcode 自己的命令清理，删除的内容无法恢复。')}
            {dev.chosen.some((r) => r.target.kind === 'docker_stopped_containers') && t('已停止容器里写过的数据会一起删掉。')}
          </p>
        </Dialog>
      )}

      {result && (
        <Dialog
          title={t('清理完成')}
          actions={
            <button className="btn" onClick={() => setResult(null)}>
              {t('好')}
            </button>
          }
        >
          <div className="group">
            <div className="row">
              <span className="grow">{t('已释放')}</span>
              <span className="num">{result.estimated ? t('约 {0}', bytes(result.freed_bytes)) : bytes(result.freed_bytes)}</span>
            </div>
            {result.trashed_bytes > 0 && (
              <div className="row">
                <span className="grow">
                  <span>{t('已移到废纸篓')}</span>
                  <small>
                    {t('清空废纸篓后才释放')}
                    {result.trashed.length > 0 && (
                      <button className="link" style={{ fontSize: 12, marginLeft: 8 }} onClick={openTrashed}>
                        {t('查看、放回原处')}
                      </button>
                    )}
                  </small>
                </span>
                <span className="num">{bytes(result.trashed_bytes)}</span>
              </div>
            )}
            {result.failed.length > 0 && (
              <div className="row">
                <span className="grow">
                  <span>{t('没能清理')}</span>
                  <small style={{ whiteSpace: 'pre-line', maxHeight: 160, overflowY: 'auto' }}>{result.failed.map((f) => t('{0}：{1}', baseName(f.path), f.reason)).join('\n')}</small>
                </span>
                <span className="num">{t('{0} 项', result.failed.length)}</span>
              </div>
            )}
            {result.warning && (
              <div className="row">
                <span className="grow">
                  <small style={{ whiteSpace: 'normal' }}>{result.warning}</small>
                </span>
              </div>
            )}
          </div>
        </Dialog>
      )}
    </>
  );
}
