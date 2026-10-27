// 仅供开发时在普通浏览器里预览界面（没有 Tauri 后端）。正式构建不包含本文件。
// fixtures/ 里的数据是后端探针命令的真实输出（进程名和路径换成了通用示例），
// 所以它同时是前后端数据格式的对照样本。
import scan from './fixtures/scan.json';
import snapshot from './fixtures/snapshot.json';
import type { AppInfo, DevEnvReport, LoginItem, RemoveResult, ScanReport, Snapshot, SpaceNode, SpaceReport } from './types';

const base = snapshot as Snapshot;
const report = scan as ScanReport;
const listeners = new Map<string, Set<(payload: unknown) => void>>();
const emit = (event: string, payload: unknown) => listeners.get(event)?.forEach((cb) => cb(payload));

const hist = { cpu: [] as number[], net: [] as number[] };
function jitter(): Snapshot {
  const wobble = (v: number, by: number) => Math.max(0, v + (Math.random() - 0.5) * by);
  const cpu = { ...base.cpu, usage_pct: Math.min(100, wobble(base.cpu.usage_pct, 20)) };
  const net = { ...base.net, down_bps: wobble(base.net.down_bps, base.net.down_bps), up_bps: wobble(base.net.up_bps, base.net.up_bps) };
  hist.cpu = [...hist.cpu, cpu.usage_pct].slice(-30);
  hist.net = [...hist.net, net.down_bps].slice(-30);
  return { ...base, ts_ms: Date.now(), cpu, net, cpu_hist: hist.cpu, net_hist: hist.net };
}
setInterval(() => emit('metrics:tick', jitter()), 2000);

const MB = 1_000_000;
const spaceNode = (path: string, mb: number, children: SpaceNode[] = [], cleanable = false, trashable = false): SpaceNode => ({
  name: path.split('/').pop() ?? '',
  path,
  size_bytes: mb * MB,
  is_dir: true,
  cleanable,
  trashable,
  children,
  other_bytes: mb * MB - children.reduce((s, c) => s + c.size_bytes, 0),
});
const spaceSample: SpaceReport = {
  root: spaceNode('/', 690_000, [
    spaceNode('/Users', 520_000, [
      spaceNode('/Users/demo', 518_000, [
        spaceNode('/Users/demo/Library', 300_000, [
          spaceNode('/Users/demo/Library/Containers', 180_000, [spaceNode('/Users/demo/Library/Containers/com.docker.docker', 64_000)]),
          spaceNode('/Users/demo/Library/Caches', 12_000, [], true),
        ]),
        spaceNode('/Users/demo/Projects', 90_000, [spaceNode('/Users/demo/Projects/old-build', 30_000, [], false, true)], false, true),
        spaceNode('/Users/demo/Downloads', 40_000, [], true),
      ]),
    ]),
    spaceNode('/Applications', 90_000, [spaceNode('/Applications/Xcode.app', 40_000)]),
    spaceNode('/opt', 25_000),
  ]),
  unreadable: ['/Users/demo/Library/Mail'],
  unreadable_count: 3,
  purgeable_bytes: 4_200 * MB,
  finished: true,
  files: 1_840_000,
  scanned_at_ms: 0,
};
let spaceDone: SpaceReport | null = null;
const GB = 1024 ** 3;
const day = 86_400_000;
const devenvSample: DevEnvReport = {
  docker: {
    state: 'ready',
    images: { size_bytes: 42_030 * MB, reclaimable_bytes: 1_106 * MB, count: 30 },
    containers: { size_bytes: 463 * MB, reclaimable_bytes: 4 * MB, count: 22 },
    volumes: { size_bytes: 1_874 * MB, reclaimable_bytes: 129 * MB, count: 14 },
    build_cache: { size_bytes: 1_467 * MB, reclaimable_bytes: 710 * MB, count: 73 },
  },
  simulators: {
    devices: [
      { udid: '0A1B2C3D-0000-4000-8000-000000000001', name: 'iPhone 17 Pro', runtime: 'com.apple.CoreSimulator.SimRuntime.iOS-26-0', os: 'iOS 26.0', available: true, size_bytes: 2_000 * MB, last_used_ms: Date.now() - 3 * day },
      { udid: '5C0983CC-6196-45A4-ABD3-5DCE776893EC', name: 'iPhone 16', runtime: 'com.apple.CoreSimulator.SimRuntime.iOS-18-3', os: 'iOS 18.3', available: false, size_bytes: 18 * MB, last_used_ms: null },
    ],
    runtimes: [
      { id: 'AAAAAAAA-0000-4000-8000-000000000001', runtime: 'com.apple.CoreSimulator.SimRuntime.iOS-26-0', os: 'iOS 26.0', version: '26.0', size_bytes: 7_500 * MB, deletable: true, in_use: true },
      { id: 'AAAAAAAA-0000-4000-8000-000000000002', runtime: 'com.apple.CoreSimulator.SimRuntime.iOS-17-5', os: 'iOS 17.5', version: '17.5', size_bytes: 6_800 * MB, deletable: true, in_use: false },
    ],
  },
};
const app = (name: string, bundle_id: string, gb: number, daysAgo: number | null, leftovers: number, running = false): AppInfo => ({
  id: `/Applications/${name}.app`,
  name,
  bundle_id,
  version: '1.0',
  path: `/Applications/${name}.app`,
  size_bytes: gb * GB,
  last_used_ms: daysAgo == null ? null : Date.now() - daysAgo * day,
  created_ms: Date.now() - (daysAgo ?? 30) * 3 * day,
  leftovers: Array.from({ length: leftovers }, (_, i) => ({ path: `/Users/demo/Library/Caches/${bundle_id}.${i}`, size_bytes: null })),
  leftover_bytes: null,
  leftover_complete: false,
  kept: [],
  running,
  tools_running: [],
});
let mockLogins: LoginItem[] = [
  { name: 'Docker', kind: 'login_item', enabled: true, global: false, path: '/Applications/Docker.app', program: null, broken: false, app: null },
  { name: 'Slack', kind: 'login_item', enabled: false, global: false, path: '/Applications/Slack.app', program: null, broken: false, app: null },
  { name: 'com.example.sync', kind: 'agent', enabled: true, global: false, path: '~/Library/LaunchAgents/com.example.sync.plist', program: '/Applications/Sync.app/Contents/MacOS/sync', broken: false, app: 'Sync' },
  { name: 'com.example.updater', kind: 'agent', enabled: false, global: false, path: '~/Library/LaunchAgents/com.example.updater.plist', program: '/Applications/Old.app/Contents/MacOS/updater', broken: true, app: 'Old' },
  { name: 'com.vendor.helper', kind: 'agent', enabled: true, global: true, path: '/Library/LaunchAgents/com.vendor.helper.plist', program: null, broken: false, app: null },
  { name: 'com.vendor.daemon', kind: 'daemon', enabled: true, global: true, path: '/Library/LaunchDaemons/com.vendor.daemon.plist', program: '/Library/PrivilegedHelperTools/com.vendor.daemon', broken: false, app: null },
];
let mockApps: AppInfo[] = [
  app('Xcode', 'com.example.xcode', 12.8, 3, 4, true),
  app('Docker', 'com.example.docker', 3.4, 130, 2),
  app('微信', 'com.example.wechat', 2.1, 0, 3, true),
  app('Google Chrome', 'com.example.chrome', 1.1, 0, 2),
  app('Sketch', 'com.example.sketch', 1.0, 240, 2),
  app('Steam', 'com.example.steam', 0.7, 400, 1),
  app('Keynote 讲演', 'com.example.keynote', 0.7, null, 0),
];

let scanned = false;
let scanTimers: ReturnType<typeof setTimeout>[] = [];
let mockIgnored: string[] = [];
let consented = false;
let passwordless = false;
let dock = 'show';
let claude: unknown = { model: null, effort: null };
const mockLanguage = () => sessionStorage.getItem('mock-language');
let apiCfg: { base_url: string; model: string } | null = null;
let backend: string | null = null;

/** 模拟 AI：第一次要求告知同意，之后等 1.5 秒返回一段示例文字。 */
function fakeAi(text: string) {
  if (!consented) return Promise.reject({ kind: 'consent_required' });
  return new Promise((resolve) => setTimeout(() => resolve({ text, cached: false, backend: 'claude_cli' }), 1500));
}

function removeItems(ids: string[]): RemoveResult {
  const out: RemoveResult = { freed_bytes: 0, trashed_bytes: 0, failed: [], estimated: false, trashed: [], warning: null };
  for (const c of report.categories) {
    for (const i of c.items.filter((x) => ids.includes(x.id))) {
      if (c.removal === 'trash') out.trashed_bytes += i.size_bytes;
      else out.freed_bytes += i.size_bytes;
    }
  }
  return out;
}

export function call(cmd: string, args?: Record<string, unknown>): unknown {
  switch (cmd) {
    case 'metrics_latest':
      return jitter();
    case 'device_info':
      return { chip: 'Apple M2 Pro', mem_total_bytes: base.mem.total_bytes, os_version: '26.7.1' };
    case 'care_report':
      return {
        battery: { health_pct: 87, cycles: 220, design_cycles: 1000, design_mah: 8694, max_mah: 7104, temp_c: 30.7, pct: 80, plugged: true, charging: false },
        disk: { used_bytes: base.disk.used_bytes, total_bytes: base.disk.total_bytes, smart: { health_pct: 88, temp_c: 38, spare_pct: 100, read_bytes: 265.9e12, written_bytes: 155.1e12, power_on_hours: 6509, unsafe_shutdowns: 18 } },
        energy: [
          { name: 'Google Chrome', pid: 519, value: 4.4e11, app: true },
          { name: 'Finder', pid: 657, value: 3.6e11, app: true },
          { name: 'rapportd', pid: 488, value: 2.1e11, app: false },
        ],
        writes: [
          { name: 'Docker', pid: 6277, value: 77.6e9, app: true },
          { name: 'Google Chrome', pid: 519, value: 14.9e9, app: true },
          { name: 'ChatGPT', pid: 639, value: 6.1e9, app: true },
        ],
      };
    case 'care_quit':
      return undefined;
    case 'scan_latest':
      return scanned ? report : null;
    case 'scan_start':
      scanTimers = report.categories.map((c, i) =>
        setTimeout(() => emit('scan:progress', { category: c.id, done: i, total: report.categories.length }), i * 250),
      );
      scanTimers.push(
        setTimeout(() => {
          scanned = true;
          emit('scan:done', report);
        }, report.categories.length * 250),
      );
      return true;
    case 'scan_cancel':
      scanTimers.forEach(clearTimeout);
      scanTimers = [];
      emit('scan:cancelled', null);
      return;
    case 'remove_items':
      return removeItems(args!.ids as string[]);
    case 'trash_last':
      return [];
    case 'ignored_list':
      return mockIgnored;
    case 'ignore_add': {
      const item = report.categories.flatMap((c) => c.items).find((i) => i.id === args!.id);
      if (item && !mockIgnored.includes(item.path)) mockIgnored = [...mockIgnored, item.path];
      return mockIgnored;
    }
    case 'ignore_remove':
      return (mockIgnored = mockIgnored.filter((p) => p !== args!.path));
    case 'full_disk_access':
      return false;
    case 'apps_list':
      return new Promise((resolve) => setTimeout(() => resolve(mockApps), 500));
    case 'apps_measure': {
      const a = mockApps.find((x) => x.id === args!.id)!;
      Object.assign(a, { leftover_bytes: a.leftovers.length * 0.2 * GB, leftover_complete: true });
      return new Promise((resolve) => setTimeout(() => resolve({ ...a }), 300));
    }
    case 'apps_uninstall': {
      const ids = args!.ids as string[];
      const gone = mockApps.filter((a) => ids.includes(a.id));
      mockApps = mockApps.filter((a) => !ids.includes(a.id));
      return { freed_bytes: 0, trashed_bytes: gone.reduce((s, a) => s + a.size_bytes + (a.leftover_bytes ?? 0), 0), failed: [] };
    }
    case 'apps_orphans':
      return new Promise((resolve) => setTimeout(() => resolve([{ path: '/Users/demo/Library/Containers/com.gone.app', size_bytes: 0.3 * GB }]), 500));
    case 'apps_trash_orphans':
      return { freed_bytes: 0, trashed_bytes: 0.3 * GB, failed: [] };
    case 'login_items_stale':
      return mockLogins.filter((l) => l.kind === 'agent' && l.broken && l.enabled).map((l) => ({ ...l }));
    case 'login_items':
      return { items: mockLogins.map((l) => ({ ...l })), apps_unreadable: false };
    case 'login_item_set':
      mockLogins.find((l) => l.kind === args!.kind && l.name === args!.name)!.enabled = args!.on as boolean;
      return undefined;
    case 'login_item_remove':
      mockLogins = mockLogins.filter((l) => !(l.kind === args!.kind && l.name === args!.name));
      return undefined;
    case 'login_item_add':
      mockLogins.unshift({ name: 'Notes', kind: 'login_item', enabled: true, global: false, path: '/System/Applications/Notes.app', program: null, broken: false, app: null });
      return true;
    case 'plugin:app|version':
      return '0.1.0';
    case 'update_check':
      return null;
    case 'settings_get':
      return { autostart: false, ai_consent: consented, dock, ai_backends: apiCfg ? ['claude_cli', 'api_key'] : ['claude_cli'], ai_backend: backend, claude, api: apiCfg, api_key_set: !!apiCfg, onboarded: true, low_disk_alert: true, menubar: { cpu: true, mem: false, disk: false }, full_disk_access: false, version: '0.1.0', language: mockLanguage(), lang: mockLanguage() ?? 'zh-Hans' };
    case 'autostart_set':
      return args!.on;
    case 'ai_claude_set':
      claude = args;
      return undefined;
    case 'ai_claude_models':
      return [
        { value: null, name: 'Opus 5.5' },
        { value: 'opus', name: 'Opus 5.5' },
        { value: 'fable', name: 'Fable 5.1' },
        { value: 'sonnet', name: 'Sonnet 5.5' },
        { value: 'haiku', name: 'Haiku 5.5' },
        { value: 'claude-opus-4-8', name: 'Opus 4.8' },
      ];
    case 'language_set':
      // 存在浏览器里：切换语言会重新加载页面，内存里的变量留不住
      if (args!.language) sessionStorage.setItem('mock-language', args!.language as string);
      else sessionStorage.removeItem('mock-language');
      location.reload();
      return undefined;
    case 'dock_mode_set':
      dock = args!.mode as string;
      return undefined;
    case 'ai_consent_revoke':
      consented = false;
      return undefined;
    case 'ai_explain_uninstall':
      return fakeAi('（预览用的示例回答）卸载会一并移除它的缓存和偏好设置。如果里面有本地数据库或容器数据，请先导出。');
    case 'ai_plan':
      if (!consented) return Promise.reject({ kind: 'consent_required' });
      return new Promise((resolve) =>
        setTimeout(() => resolve({ categories: ['app_cache', 'logs'], reason: '（预览用的示例回答）只选了缓存和日志，下载文件夹里可能有你自己的文件，没有选。', cached: false, backend: 'claude_cli' }), 1500),
      );
    case 'ai_api_set':
      if (!String(args!.baseUrl).startsWith('https://')) return Promise.reject('服务地址必须以 https:// 开头（本机地址除外）');
      apiCfg = { base_url: args!.baseUrl as string, model: args!.model as string };
      return undefined;
    case 'ai_api_clear':
      apiCfg = null;
      backend = null;
      return undefined;
    case 'ai_backend_set':
      backend = args!.backend as string | null;
      return undefined;
    case 'item_reveal':
    case 'app_reveal':
      return undefined;
    case 'ai_organize_downloads':
      if (!consented) return Promise.reject({ kind: 'consent_required' });
      return new Promise((resolve) =>
        setTimeout(
          () =>
            resolve({
              groups: [
                { title: '安装包', advice: 'delete', why: '对应的应用已安装', item_ids: ['x'], total_bytes: 5.2 * 1024 ** 2 },
                { title: '票据与合同', advice: 'keep', why: '可能以后还要用', item_ids: ['y', 'z'], total_bytes: 0.4 * 1024 ** 2 },
              ],
              cached: false,
              backend: 'claude_cli',
            }),
          1500,
        ),
      );
    case 'onboarded_set':
    case 'low_disk_alert_set':
    case 'menubar_set':
      return undefined;
    case 'ai_diagnose':
      return fakeAi('（预览用的示例回答）内存压力偏高，主要来自浏览器和开发工具，可以关掉不用的标签页；磁盘已用 76%，暂时不影响速度；开机自启的后台项目有两个长期未用，建议在系统设置里关掉。');
    case 'week_report':
      return { journal: { cleaned_bytes: 3.1e9, uninstalled: 1, optimized: 2 }, disk_delta_bytes: 4.2e9 };
    case 'journal_summary':
      return { cleaned_bytes: 8.2 * 1024 ** 3, uninstalled: 3, optimized: 4 };
    case 'awake_get':
      return false;
    case 'keyboard_clean_start':
      return 30;
    case 'keyboard_clean_stop':
    case 'open_accessibility_settings':
    case 'open_sponsor_page':
      return undefined;
    case 'awake_set':
      return args!.on;
    case 'volumes_list':
      return ['移动硬盘'];
    case 'admin_passwordless':
      return { on: passwordless, tasks: ['purge_memory', 'flush_dns'] };
    case 'admin_passwordless_set':
      passwordless = args!.on as boolean;
      return undefined;
    case 'volume_eject':
    case 'mem_free':
      return Math.round(3e9 + Math.random() * 1e9);
    case 'apps_quit':
      return new Promise((resolve) => setTimeout(() => resolve([]), 1200));
    case 'process_quit':
    case 'admin_run':
      return new Promise((resolve) => setTimeout(resolve, 600));
    case 'ai_backends':
      return ['claude_cli'];
    case 'ai_consent':
      consented = true;
      return undefined;
    case 'devenv_report':
      return devenvSample;
    case 'devenv_clean':
      return { freed_bytes: 710 * MB, trashed_bytes: 0, failed: [], estimated: false, trashed: [], warning: null };
    case 'space_start':
      [1, 2, 3].forEach((i) =>
        setTimeout(
          () =>
            emit('space:progress', {
              files: i * 400_000,
              bytes: i * 150_000 * MB,
              tops: [
                { path: '/Users/demo/Library', bytes: i * 90_000 * MB, done: i === 3 },
                { path: '/Applications', bytes: 60_000 * MB, done: true },
                { path: '/Users/demo/code', bytes: i * 15_000 * MB, done: false },
                { path: '/Library', bytes: 12_000 * MB, done: true },
              ],
            }),
          i * 300,
        ),
      );
      setTimeout(() => emit('space:done', (spaceDone = { ...spaceSample, scanned_at_ms: Date.now() })), 1200);
      return true;
    case 'space_latest':
      return spaceDone;
    case 'space_trash': {
      // 预览里只把这一项从树里拿掉、大小记进上级的「其他」，不算真正的废纸篓
      const drop = (n: SpaceNode): SpaceNode => {
        const gone = n.children.find((c) => c.path === args!.path);
        return gone ? { ...n, children: n.children.filter((c) => c !== gone), other_bytes: n.other_bytes + gone.size_bytes } : { ...n, children: n.children.map(drop) };
      };
      return (spaceDone = { ...spaceDone!, root: drop(spaceDone!.root) });
    }
    case 'space_cancel':
    case 'space_reveal':
      return undefined;
    case 'usage_trend':
      return { points: [{ day: 20000, used_bytes: 380 * GB, total_bytes: 500 * GB }, { day: 20027, used_bytes: 401 * GB, total_bytes: 500 * GB }], bytes_per_day: 0.78 * GB, days_to_full: 128 };
    case 'ai_explain_trend':
      return fakeAi('（预览用的示例回答）近 4 周用量稳步上涨，每天约多用 0.8 GB，常见原因是照片同步、下载文件和开发工具缓存。可以先看看「空间去向」里增长最快的文件夹。');
    case 'ai_explain_space':
      return fakeAi('（预览用的示例回答）这是 Docker 的数据目录，里面是镜像、容器和数据卷。不要直接删除，应在 Docker 里清理不用的镜像和构建缓存。');
    case 'ai_explain_login_item':
      return fakeAi('（预览用的示例回答）这是某个应用的后台更新程序，开机后在后台检查新版本。关掉后不影响应用使用，只是要手动检查更新。');
    case 'ai_explain_item':
      return fakeAi('（预览用的示例回答）这是某个应用的缓存目录，用来加快再次加载的速度。删除后应用会在需要时重新生成，不会丢失账号和个人文件，可以放心删除。');
    case 'ai_explain_process':
      return fakeAi('（预览用的示例回答）浏览器的每个标签页和扩展都是独立进程，这里是合计。关闭不用的标签页最有效，可以正常退出。');
    case 'ai_summarize_scan':
      return fakeAi('（预览用的示例回答）空间主要被应用缓存和下载文件夹里的旧文件占用，清理默认勾选的类别能腾出约 13 MB，不会影响个人文件。');
    default:
      console.info('[预览模式] 忽略命令', cmd, args);
      return undefined;
  }
}

export function on(event: string, cb: (payload: unknown) => void): () => void {
  if (!listeners.has(event)) listeners.set(event, new Set());
  listeners.get(event)!.add(cb);
  return () => listeners.get(event)!.delete(cb);
}
