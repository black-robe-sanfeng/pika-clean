// 前端与后端通信的唯一出口：所有 Tauri 命令和事件都从这里走。接口清单见 docs/api/README.md。
import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { useEffect, useState, useSyncExternalStore } from 'react';
import type { Lang } from '../lib/i18n.ts';
import type { AdminTask, AiBackend, Answer, AppInfo, AppUpdate, CareReport, ClaudeModel, CleanProgress, DevEnvReport, DevTarget, DeviceInfo, DockMode, Effort, JournalSummary, MenubarShow, Leftover, LoginItem, LoginItems, Organized, Passwordless, Plan, Progress, RemoveResult, ScanReport, SettingsView, Snapshot, SpaceProgress, SpaceReport, Trashed, UninstallProgress, UpdateInfo, UsageTrend, WeekReport } from './types';
import { t } from '../lib/i18n.ts';

export type * from './types';

export const inTauri = '__TAURI_INTERNALS__' in window;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (inTauri) return invoke<T>(cmd, args);
  // 仅开发时在普通浏览器里预览界面用；正式构建里这段会被整段剔除
  if (import.meta.env.DEV) return (await import('./mock')).call(cmd, args) as T;
  throw new Error(t('不在应用内，无法调用后端'));
}

/** 回答每生成一段就调用一次；最终的完整回答仍然是命令的返回值。 */
export type OnDelta = (chunk: string) => void;

/** 每次提问的 onDelta 对应的通道编号，也就是后端认的提问编号，`aiCancel` 用它。 */
const asking = new WeakMap<OnDelta, number>();

/**
 * 会一边生成一边送回文字的 AI 命令。浏览器预览时没有后端，直接返回整段示例回答。
 * fresh 为真时后端不用缓存，重新回答。
 */
function stream<T>(cmd: string, args: Record<string, unknown>, onDelta?: OnDelta, fresh?: boolean): Promise<T> {
  if (!inTauri) return call<T>(cmd, args);
  const channel = new Channel<string>();
  if (onDelta) {
    channel.onmessage = onDelta;
    asking.set(onDelta, channel.id);
  }
  return invoke<T>(cmd, { ...args, onDelta: channel, fresh });
}

/** 不要这个回答了（界面关了、重新提问了）：让后端停下这次提问，Claude Code 的子进程会被结束。 */
export function aiCancel(onDelta: OnDelta) {
  const id = asking.get(onDelta);
  if (id !== undefined) call<void>('ai_cancel', { id }).catch(() => {});
}

function on<T>(event: string, cb: (payload: T) => void): () => void {
  let stop = () => {};
  let cancelled = false;
  const attach = inTauri
    ? listen<T>(event, (e) => cb(e.payload))
    : import.meta.env.DEV
      ? import('./mock').then((m) => m.on(event, cb as (p: unknown) => void))
      : Promise.resolve(() => {});
  attach.then((un) => (cancelled ? un() : (stop = un)));
  return () => {
    cancelled = true;
    stop();
  };
}

export const api = {
  metricsLatest: () => call<Snapshot | null>('metrics_latest'),
  deviceInfo: () => call<DeviceInfo>('device_info'),
  showMain: (view?: string) => call<void>('show_main', { view }),
  popoverResize: (height: number) => call<void>('popover_resize', { height }),
  scanStart: () => call<boolean>('scan_start'),
  /** 停止正在进行的扫描，随后会收到 onScanCancelled */
  scanCancel: () => call<void>('scan_cancel'),
  scanLatest: () => call<ScanReport | null>('scan_latest'),
  removeItems: (ids: string[], dryRun: boolean) => call<RemoveResult>('remove_items', { ids, dryRun }),
  /** 让正在进行的清理（remove_items、devenv_clean）在处理完手上这一项后停下 */
  cleanCancel: () => call<void>('clean_cancel'),
  /** 最近一次清理移到废纸篓的条目 */
  trashLast: () => call<Trashed[]>('trash_last'),
  trashPutBack: (to: string) => call<void>('trash_put_back', { to }),
  trashReveal: (to: string) => call<void>('trash_reveal', { to }),
  /** 「不再提示」的条目路径；增删后返回新的名单 */
  ignoredList: () => call<string[]>('ignored_list'),
  ignoreAdd: (id: string) => call<string[]>('ignore_add', { id }),
  ignoreRemove: (path: string) => call<string[]>('ignore_remove', { path }),
  fullDiskAccess: () => call<boolean>('full_disk_access'),
  journalSummary: () => call<JournalSummary>('journal_summary'),
  weekReport: () => call<WeekReport | null>('week_report'),
  awakeGet: () => call<boolean>('awake_get'),
  awakeSet: (on: boolean) => call<boolean>('awake_set', { on }),
  /** 锁住键盘若干秒，返回实际秒数。没有「辅助功能」权限时以字符串 no_access 拒绝 */
  keyboardCleanStart: (seconds: number) => call<number>('keyboard_clean_start', { seconds }),
  keyboardCleanStop: () => call<void>('keyboard_clean_stop'),
  openAccessibilitySettings: () => call<void>('open_accessibility_settings'),
  volumesList: () => call<string[]>('volumes_list'),
  volumeEject: (name: string) => call<void>('volume_eject', { name }),
  processQuit: (name: string) => call<void>('process_quit', { name }),
  careReport: () => call<CareReport>('care_report'),
  /** 只退出后端重新算出的耗电、写盘榜上的应用 */
  careQuit: (name: string) => call<void>('care_quit', { name }),
  /** 此刻的空闲内存（字节）；「释放内存」前后各量一次，算出实际腾出了多少 */
  memFree: () => call<number | null>('mem_free'),
  /** 请这几个应用退出并等它们退出，返回没有退出的名字 */
  appsQuit: (names: string[]) => call<string[]>('apps_quit', { names }),
  openFullDiskAccessSettings: () => call<void>('open_full_disk_access_settings'),
  openSponsorPage: () => call<void>('open_sponsor_page'),
  adminRun: (task: AdminTask) => call<void>('admin_run', { task }),
  adminPasswordless: () => call<Passwordless>('admin_passwordless'),
  adminPasswordlessSet: (on: boolean) => call<void>('admin_passwordless_set', { on }),
  appsList: () => call<AppInfo[]>('apps_list'),
  appsMeasure: (id: string) => call<AppInfo>('apps_measure', { id }),
  appReveal: (id: string) => call<void>('app_reveal', { id }),
  appOpen: (id: string) => call<void>('app_open', { id }),
  appsUpdates: () => call<AppUpdate[]>('apps_updates'),
  appsUninstall: (ids: string[], dryRun: boolean) => call<RemoveResult>('apps_uninstall', { ids, dryRun }),
  /** 找已删除应用留下的数据，已量好大小 */
  appsOrphans: () => call<Leftover[]>('apps_orphans'),
  /** 只认后端重新找出来仍在名单里的路径 */
  appsTrashOrphans: (paths: string[]) => call<RemoveResult>('apps_trash_orphans', { paths }),
  devenvReport: () => call<DevEnvReport>('devenv_report'),
  devenvClean: (targets: DevTarget[], dryRun: boolean) => call<RemoveResult>('devenv_clean', { targets, dryRun }),
  /** 有新版本时返回它的版本号和更新说明，没有返回空 */
  updateCheck: () => call<UpdateInfo | null>('update_check'),
  /** 下载安装新版本并重启应用；装上了就不会返回，返回了说明服务器上已经没有新版本。下载进度（0~100）交给 onProgress */
  updateInstall: (onProgress: (percent: number) => void) => {
    if (!inTauri) return call<void>('update_install');
    const channel = new Channel<number>();
    channel.onmessage = onProgress;
    return invoke<void>('update_install', { onProgress: channel });
  },
  loginItems: () => call<LoginItems>('login_items'),
  loginItemsStale: () => call<LoginItem[]>('login_items_stale'),
  loginItemSet: (item: LoginItem, on: boolean) => call<void>('login_item_set', { kind: item.kind, name: item.name, on }),
  loginItemRemove: (item: LoginItem) => call<void>('login_item_remove', { kind: item.kind, name: item.name }),
  /** 弹出系统的选择应用窗口；返回 false 表示用户取消了 */
  loginItemAdd: () => call<boolean>('login_item_add'),
  openLoginItemsSettings: () => call<void>('open_login_items_settings'),
  aiExplainLoginItem: (item: LoginItem, onDelta?: OnDelta, fresh?: boolean) => stream<Answer>('ai_explain_login_item', { kind: item.kind, name: item.name }, onDelta, fresh),
  settingsGet: () => call<SettingsView>('settings_get'),
  autostartSet: (on: boolean) => call<boolean>('autostart_set', { on }),
  dockModeSet: (mode: DockMode) => call<void>('dock_mode_set', { mode }),
  /** 切换界面语言；null 表示跟随系统。成功后各窗口会收到 onLanguageChange */
  languageSet: (language: Lang | null) => call<void>('language_set', { language }),
  aiConsentRevoke: () => call<void>('ai_consent_revoke'),
  aiExplainUninstall: (id: string, onDelta?: OnDelta, fresh?: boolean) => stream<Answer>('ai_explain_uninstall', { id }, onDelta, fresh),
  aiDiagnose: (onDelta?: OnDelta, fresh?: boolean) => stream<Answer>('ai_diagnose', {}, onDelta, fresh),
  aiPlan: (request: string, fresh?: boolean) => call<Plan>('ai_plan', { request, fresh }),
  aiApiSet: (baseUrl: string, model: string, key: string | null) => call<void>('ai_api_set', { baseUrl, model, key }),
  aiApiClear: () => call<void>('ai_api_clear'),
  aiBackendSet: (backend: AiBackend | null) => call<void>('ai_backend_set', { backend }),
  aiClaudeSet: (model: string | null, effort: Effort | null) => call<void>('ai_claude_set', { model, effort }),
  /** 要启动一次 Claude Code，耗时几秒 */
  aiClaudeModels: () => call<ClaudeModel[]>('ai_claude_models'),
  aiOrganizeDownloads: (fresh?: boolean) => call<Organized>('ai_organize_downloads', { fresh }),
  onboardedSet: () => call<void>('onboarded_set'),
  lowDiskAlertSet: (on: boolean) => call<void>('low_disk_alert_set', { on }),
  hotkeySet: (on: boolean) => call<void>('hotkey_set', { on }),
  cleanReminderSet: (on: boolean) => call<void>('clean_reminder_set', { on }),
  menubarSet: (menubar: MenubarShow) => call<void>('menubar_set', { menubar }),
  itemReveal: (id: string) => call<void>('item_reveal', { id }),
  aiBackends: () => call<string[]>('ai_backends'),
  aiConsent: () => call<void>('ai_consent'),
  aiExplainItem: (id: string, onDelta?: OnDelta, fresh?: boolean) => stream<Answer>('ai_explain_item', { id }, onDelta, fresh),
  aiExplainProcess: (name: string, onDelta?: OnDelta, fresh?: boolean) => stream<Answer>('ai_explain_process', { name }, onDelta, fresh),
  aiSummarizeScan: (onDelta?: OnDelta, fresh?: boolean) => stream<Answer>('ai_summarize_scan', {}, onDelta, fresh),
  spaceStart: () => call<boolean>('space_start'),
  spaceCancel: () => call<void>('space_cancel'),
  spaceLatest: () => call<SpaceReport | null>('space_latest'),
  spaceReveal: (path: string) => call<void>('space_reveal', { path }),
  spaceTrash: (path: string) => call<SpaceReport>('space_trash', { path }),
  aiExplainSpace: (path: string, onDelta?: OnDelta, fresh?: boolean) => stream<Answer>('ai_explain_space', { path }, onDelta, fresh),
  /** 最近 28 天的启动盘用量趋势；记录不足两天时为空 */
  usageTrend: () => call<UsageTrend | null>('usage_trend'),
  aiExplainTrend: (onDelta?: OnDelta, fresh?: boolean) => stream<Answer>('ai_explain_trend', {}, onDelta, fresh),
  onSpaceProgress: (cb: (p: SpaceProgress) => void) => on<SpaceProgress>('space:progress', cb),
  onSpaceDone: (cb: (r: SpaceReport) => void) => on<SpaceReport>('space:done', cb),
  onMetrics: (cb: (s: Snapshot) => void) => on<Snapshot>('metrics:tick', cb),
  onScanProgress: (cb: (p: Progress) => void) => on<Progress>('scan:progress', cb),
  onScanDone: (cb: (r: ScanReport) => void) => on<ScanReport>('scan:done', cb),
  onScanCancelled: (cb: () => void) => on<null>('scan:cancelled', cb),
  /** 清理后不整份重扫：去掉已删条目、只重扫个别类别后的报告 */
  onScanUpdate: (cb: (r: ScanReport) => void) => on<ScanReport>('scan:update', cb),
  onCleanProgress: (cb: (p: CleanProgress) => void) => on<CleanProgress>('clean:progress', cb),
  onUninstallProgress: (cb: (p: UninstallProgress) => void) => on<UninstallProgress>('uninstall:progress', cb),
  onNavigate: (cb: (view: string) => void) => on<string>('nav:go', cb),
  onLanguageChange: (cb: (lang: string) => void) => on<string>('lang:changed', cb),
};

/**
 * 最新的系统指标快照；后端还没采到第一份时为 null。
 * `active` 为 false 时不订阅（主窗口里藏起来的页面也挂着，不该每 2 秒白渲染一次），切回来先补一份最新的。
 * 后端只给看得见的窗口推送，所以窗口重新显示时也先补一份，不显示收起前的旧数。
 */
export function useSnapshot(active = true): Snapshot | null {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  useEffect(() => {
    if (!active) return;
    const latest = () => api.metricsLatest().then((s) => s && setSnap((cur) => (cur && cur.ts_ms >= s.ts_ms ? cur : s)));
    const onShow = () => document.visibilityState === 'visible' && latest();
    latest();
    document.addEventListener('visibilitychange', onShow);
    const off = api.onMetrics(setSnap);
    return () => {
      document.removeEventListener('visibilitychange', onShow);
      off();
    };
  }, [active]);
  return snap;
}

// AI 能不能用：每个窗口一份，所有入口共用。设置里改了（后端发 ai:changed）或窗口重新获得焦点
// （可能刚装好或卸掉 Claude Code）时重新查一次。
let aiAvailable = false;
const aiListeners = new Set<() => void>();
function refreshAi() {
  api.aiBackends().then((b) => {
    if (aiAvailable === b.length > 0) return;
    aiAvailable = b.length > 0;
    aiListeners.forEach((l) => l());
  });
}
let aiWatching = false;
function subscribeAi(listener: () => void) {
  aiListeners.add(listener);
  if (!aiWatching) {
    aiWatching = true;
    refreshAi();
    on('ai:changed', refreshAi);
    window.addEventListener('focus', refreshAi);
  }
  return () => aiListeners.delete(listener);
}

/** 只要启动盘的数字。空闲块随时有零星写入，变化不到 10 MB 时不更新，免得只用磁盘的页面每 2 秒白渲染。 */
export function useDisk(): Snapshot['disk'] | null {
  const [disk, setDisk] = useState<Snapshot['disk'] | null>(null);
  useEffect(() => {
    const put = (s: Snapshot | null) =>
      s && setDisk((d) => (d && d.total_bytes === s.disk.total_bytes && Math.abs(d.used_bytes - s.disk.used_bytes) < 10_000_000 ? d : s.disk));
    api.metricsLatest().then(put);
    return api.onMetrics(put);
  }, []);
  return disk;
}

/** 有没有可用的 AI 后端。没有时所有 AI 入口都不显示。 */
export function useAiAvailable(): boolean {
  return useSyncExternalStore(subscribeAi, () => aiAvailable);
}

/** 右键菜单：「在访达中显示」加上 `extra` 里的几项，代替网页视图自带的那份（查询、翻译、检查元素）。浏览器预览时没有原生菜单，直接执行第一项。 */
export async function revealMenu(reveal: () => void, extra: { text: string; action: () => void }[] = []) {
  if (!inTauri) return reveal();
  const { Menu } = await import('@tauri-apps/api/menu');
  const menu = await Menu.new({ items: [{ id: 'reveal', text: t('在访达中显示'), action: reveal }, ...extra.map((x, i) => ({ id: `extra${i}`, text: x.text, action: x.action }))] });
  await menu.popup();
}
