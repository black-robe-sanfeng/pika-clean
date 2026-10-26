import { Fragment, useEffect, useRef, useState } from 'react';
import { api, type DevEnvReport, type DevTarget } from '../api';
import { Check } from './Check';
import { Icon, type IconName, Tile } from './Icon';
import { Sheet } from './Sheet';
import { ago, bytes } from '../lib/fmt';
import { t } from '../lib/i18n.ts';

/** 确认框里给人看的命令。真正执行的写死在后端 remove::command_for，两边要保持一致 */
export const DEV_COMMAND: Record<DevTarget['kind'], string> = {
  docker_build_cache: 'docker builder prune -a -f',
  docker_stopped_containers: 'docker container prune -f',
  docker_unused_images: 'docker image prune -a -f',
  sim_unavailable: 'xcrun simctl delete unavailable',
  sim_device: 'xcrun simctl delete',
  sim_runtime: 'xcrun simctl runtime delete',
};

export type DevRow = { key: string; target: DevTarget; name: string; desc: string; bytes: number; blocked?: string };

/**
 * 清理页里「开发环境」的数据和勾选：Docker、iOS 模拟器（规格切片 17）。
 * 它们用自己的命令清理，但勾选算进清理页底部的总数，由同一个「清理」按钮一起清。
 * stamp 变了（每次完整扫描完成）或调了 reload（清理过开发环境）就重新读一遍。
 */
export function useDevEnv(stamp: unknown) {
  const [report, setReport] = useState<DevEnvReport | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  // 右侧明细面板正在看哪一行
  const [open, setOpen] = useState<'docker' | 'sims' | null>(null);
  const [reloads, setReloads] = useState(0);
  const reload = () => setReloads((n) => n + 1);
  const pick = (keys: string[], on: boolean) =>
    setPicked((p) => {
      const next = new Set(p);
      for (const k of keys) on ? next.add(k) : next.delete(k);
      return next;
    });
  useEffect(() => {
    api.devenvReport().then((r) => {
      setReport(r);
      // 只有「不可用的模拟器」默认勾选
      setPicked(new Set(r.simulators?.devices.some((d) => !d.available) ? ['sim_unavailable'] : []));
    });
  }, [stamp, reloads]);
  if (!report) return { report, chosen: [] as DevRow[], groups: 0, picked, pick, open, setOpen, reload, rows: null };
  const { docker, simulators: sims } = report;

  const dockerRows: DevRow[] =
    docker.state === 'ready'
      ? [
          { key: 'docker_build_cache', target: { kind: 'docker_build_cache' }, name: t('Docker 构建缓存'), desc: t('删了下次构建要重新下载，不影响已有镜像'), bytes: docker.build_cache.reclaimable_bytes },
          { key: 'docker_stopped_containers', target: { kind: 'docker_stopped_containers' }, name: t('Docker 已停止的容器'), desc: t('容器里写过的数据会一起删掉'), bytes: docker.containers.reclaimable_bytes },
          { key: 'docker_unused_images', target: { kind: 'docker_unused_images' }, name: t('Docker 没在用的镜像'), desc: t('没有容器在用的镜像，要用时重新下载'), bytes: docker.images.reclaimable_bytes },
        ]
      : [];
  const unavailable = sims?.devices.filter((d) => !d.available) ?? [];
  const unavailableRow: DevRow | null = unavailable.length
    ? { key: 'sim_unavailable', target: { kind: 'sim_unavailable' }, name: t('不可用的模拟器'), desc: t('{0} 台，系统版本已经没了，开不起来', unavailable.length), bytes: unavailable.reduce((s, d) => s + d.size_bytes, 0) }
    : null;
  const deviceRows: DevRow[] = (sims?.devices ?? [])
    .filter((d) => d.available)
    .map((d): DevRow => ({ key: `sim_device:${d.udid}`, target: { kind: 'sim_device', id: d.udid }, name: d.name, desc: t('{0} · 上次使用：{1}', d.os, ago(d.last_used_ms)), bytes: d.size_bytes }));
  const runtimeRows: DevRow[] = (sims?.runtimes ?? []).map((r): DevRow => ({
    key: `sim_runtime:${r.id}`,
    target: { kind: 'sim_runtime', id: r.id },
    name: r.os,
    desc: t('删了以后再用要重新下载'),
    bytes: r.size_bytes,
    blocked: r.in_use ? t('有模拟器在用') : !r.deletable ? t('随 Xcode 安装，不能单独删') : undefined,
  }));
  const all = [...dockerRows, ...(unavailableRow ? [unavailableRow] : []), ...deviceRows, ...runtimeRows];
  const chosen = all.filter((r) => picked.has(r.key));
  return {
    report,
    chosen,
    // 底部「已选 N 类」：Docker、模拟器各算一类
    groups: new Set(chosen.map((r) => (r.key.startsWith('docker') ? 'docker' : 'sims'))).size,
    picked,
    pick,
    open,
    setOpen,
    reload,
    rows: { dockerRows, unavailableRow, deviceRows, runtimeRows },
  };
}

export type DevEnv = ReturnType<typeof useDevEnv>;

/** 模拟器那一行包含的全部条目：不可用的模拟器、模拟器设备、系统版本 */
const simRows = (rows: NonNullable<DevEnv['rows']>) => [...(rows.unavailableRow ? [rows.unavailableRow] : []), ...rows.deviceRows, ...rows.runtimeRows];
type Fold = NonNullable<DevEnv['open']>;

/**
 * 开发环境的几行，放在清理页「可清理的文件」那张列表里，样式和上面的类别一样。
 * Docker、模拟器各占一行（模拟器这一行包括不可用的模拟器、模拟器设备和系统版本），点箭头在右侧面板里逐项勾选。
 * onOpen：打开面板前调用，让清理页先收起类别明细（两个面板在同一位置）。
 */
export function DevEnvRows({ dev, onOpen }: { dev: DevEnv; onOpen: () => void }) {
  const { report, rows, picked, pick } = dev;
  if (!report || !rows) return null;
  const { docker } = report;
  const { dockerRows, unavailableRow, deviceRows, runtimeRows } = rows;

  const row = (key: string, icon: IconName, color: string, name: string, desc: string, list: DevRow[], fold?: Fold) => {
    const can = list.filter((r) => !r.blocked);
    const total = list.reduce((s, r) => s + r.bytes, 0);
    const n = can.filter((r) => picked.has(r.key)).length;
    return (
      <label className={`row${total === 0 ? ' off' : ''}`} key={key}>
        <Tile name={icon} color={color} />
        <span className="grow">
          <span>
            {name}
            {total > 0 && <em className="tag">{t('需确认')}</em>}
            {n > 0 && n < can.length && <em className="tag">{t('只选了 {0} 项', n)}</em>}
          </span>
          <small>{desc}</small>
        </span>
        <span className="num">{bytes(total)}</span>
        <button
          className="aib"
          style={{ color: 'var(--sub)', background: 'var(--fill)' }}
          disabled={!fold}
          onClick={() => {
            onOpen();
            dev.setOpen(fold!);
          }}
          aria-label={t('查看{0}的明细', name)}
        >
          <Icon name="chev" />
        </button>
        <Check all={n > 0 && n === can.length} some={n > 0} disabled={total === 0 || can.length === 0} onChange={() => pick(can.map((r) => r.key), n < can.length)} />
      </label>
    );
  };

  return (
    <>
      {docker.state === 'not_running' && (
        <div className="row off">
          <Tile name="box" color="c-teal" />
          <span className="grow">
            <span>Docker</span>
            <small>{t('Docker 没在运行，打开 Docker 后重新扫描才能看到')}</small>
          </span>
        </div>
      )}
      {docker.state === 'ready' && row('docker', 'box', 'c-teal', 'Docker', t('构建缓存、已停止的容器、没在用的镜像；数据卷不清理'), dockerRows, 'docker')}
      {simRows(rows).length > 0 &&
        row('sims', 'gpu', 'c-indigo', t('模拟器'), t('{0} 台设备、{1} 个系统版本，点开逐个勾选', deviceRows.length + (unavailableRow ? 1 : 0), runtimeRows.length), simRows(rows), 'sims')}
    </>
  );
}

/** 明细面板里分几段显示：模拟器分成设备和系统版本两段，Docker 一段 */
function sections(fold: Fold | null, list: DevRow[]) {
  if (fold !== 'sims') return [{ title: '', items: list }];
  const runtime = (r: DevRow) => r.key.startsWith('sim_runtime');
  return [
    { title: t('模拟器设备'), items: list.filter((r) => !runtime(r)) },
    { title: t('系统版本'), items: list.filter(runtime) },
  ].filter((s) => s.items.length > 0);
}

/** 开发环境某一行的明细面板：逐项勾选。和类别明细共用右侧面板的位置。 */
export function DevEnvSheet({ dev }: { dev: DevEnv }) {
  // 关闭动画期间继续显示上一次的内容
  const shown = useRef<Fold | null>(null);
  if (dev.open) shown.current = dev.open;
  const fold = shown.current;
  const { report, rows, picked, pick } = dev;
  if (!report || !rows) return null;
  const { docker } = report;
  const list = fold === 'docker' ? rows.dockerRows : fold === 'sims' ? simRows(rows) : [];
  const can = list.filter((r) => !r.blocked);
  const mine = can.filter((r) => picked.has(r.key));
  return (
    <Sheet
      open={dev.open != null}
      title={fold === 'docker' ? 'Docker' : fold === 'sims' ? t('模拟器') : ''}
      onClose={() => dev.setOpen(null)}
      footer={fold === 'docker' ? t('Docker 的磁盘镜像文件删完内容后不会自动变小，需要在 Docker 设置里调小') : t('用 Docker 和 Xcode 自己的命令清理，删除的内容无法恢复。')}
    >
      <p>
        {t('{0} 项 · {1}', list.length, bytes(list.reduce((s, r) => s + r.bytes, 0)))}
        {t('，已选 {0} 项 {1}', mine.length, bytes(mine.reduce((s, r) => s + r.bytes, 0)))}
        {can.length > 0 && (
          <button className="link" style={{ fontSize: 13, marginLeft: 10 }} onClick={() => pick(can.map((r) => r.key), mine.length < can.length)}>
            {mine.length < can.length ? t('全选') : t('全不选')}
          </button>
        )}
      </p>
      {sections(fold, list).map((s) => (
        <Fragment key={s.title}>
          {s.title && <div className="gh">{s.title}</div>}
          <div className="group">
            {s.items.map((r) => (
              <label className={`row${r.blocked ? ' off' : ''}`} key={r.key}>
                <span className="grow">
                  <span>
                    {r.name}
                    {r.blocked && <em className="tag">{r.blocked}</em>}
                  </span>
                  <small>{r.desc}</small>
                </span>
                <span className="num">{bytes(r.bytes)}</span>
                <Check all={picked.has(r.key)} disabled={!!r.blocked} onChange={() => pick([r.key], !picked.has(r.key))} />
              </label>
            ))}
            {fold === 'docker' && docker.state === 'ready' && (
              <div className="row off">
                <span className="grow">
                  <span>
                    {t('Docker 数据卷')}
                    <em className="tag">{t('不清理')}</em>
                  </span>
                  <small>{t('里面是数据库之类的数据，只显示大小')}</small>
                </span>
                <span className="num">{bytes(docker.volumes.size_bytes)}</span>
              </div>
            )}
          </div>
        </Fragment>
      ))}
    </Sheet>
  );
}
