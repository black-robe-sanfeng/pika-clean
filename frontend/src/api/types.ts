import type { Lang } from '../lib/i18n.ts';

// 与 backend/src/model.rs 一一对应。改一边必须同步另一边和 docs/api/README.md。

export type Pressure = 'normal' | 'warn' | 'critical';

export interface Snapshot {
  ts_ms: number;
  uptime_s: number;
  cpu: { usage_pct: number; load1: number; cores: number; temp_c: number | null };
  gpu: { usage_pct: number; temp_c: number | null } | null;
  mem: { used_bytes: number; total_bytes: number; pressure: Pressure; swap_used_bytes: number; swap_total_bytes: number };
  /** health_pct：内置固态硬盘的健康度（100 减已用寿命），读不到时为 null */
  disk: { used_bytes: number; total_bytes: number; health_pct: number | null };
  net: { down_bps: number; up_bps: number; iface: string };
  battery: { pct: number; charging: boolean; health_pct: number | null; cycles: number | null } | null;
  fan: { rpm: number; count: number } | null;
  mic_in_use: boolean | null;
  camera_in_use: boolean | null;
  top: Proc[];
  /** 最近 30 次采样的 CPU 占用和下行网速，旧的在前；没有窗口可见时也照样记 */
  cpu_hist: number[];
  net_hist: number[];
}

export interface Proc {
  name: string;
  pid: number;
  mem_bytes: number;
  cpu_pct: number;
}

/** 保养页的体检结果，对应 model.rs 的 CareReport */
export interface CareReport {
  battery: {
    health_pct: number | null;
    cycles: number | null;
    design_cycles: number | null;
    design_mah: number | null;
    max_mah: number | null;
    temp_c: number | null;
    pct: number | null;
    plugged: boolean;
    charging: boolean;
  } | null;
  disk: {
    used_bytes: number;
    total_bytes: number;
    smart: { health_pct: number; temp_c: number | null; spare_pct: number; read_bytes: number; written_bytes: number; power_on_hours: number; unsafe_shutdowns: number } | null;
  };
  /** 值是纳焦，只看相对大小 */
  energy: Hog[];
  /** 值是字节 */
  writes: Hog[];
}

export interface Hog {
  name: string;
  pid: number;
  value: number;
  /** 是 .app 应用才能退出 */
  app: boolean;
}

export interface DeviceInfo {
  chip: string;
  mem_total_bytes: number;
  os_version: string;
}

export type CategoryId =
  | 'app_cache'
  | 'system_cache'
  | 'trash'
  | 'old_downloads'
  | 'logs'
  | 'dev_cache'
  | 'stale_projects'
  | 'ai_cache'
  | 'ai_history'
  | 'crash_dumps'
  | 'ios_backups'
  | 'xcode_archives'
  | 'ios_firmware'
  | 'mail_downloads'
  | 'homebrew_old'
  | 'duplicates'
  | 'large_files';

export interface Item {
  /** 条目编号。删除时只传它，不传路径。 */
  id: string;
  path: string;
  size_bytes: number;
  last_used_ms: number | null;
  /** 限时内没量完，size_bytes 只是下限 */
  partial: boolean;
}

export interface Category {
  id: CategoryId;
  root: string;
  total_bytes: number;
  items: Item[];
  default_selected: boolean;
  /** permanent：直接删除；trash：移到废纸篓 */
  removal: 'permanent' | 'trash';
  needs_admin: boolean;
  needs_full_disk_access: boolean;
  unreadable: boolean;
  /** 有条目限时内没量完，total_bytes 只是下限 */
  partial: boolean;
}

export interface ScanReport {
  categories: Category[];
  /** 扫描开始的时间（毫秒时间戳） */
  at_ms?: number;
}

export interface Progress {
  category: CategoryId;
  done: number;
  total: number;
}

export interface RemoveResult {
  /** freed_bytes 是估计值，界面写「大约」 */
  estimated: boolean;
  freed_bytes: number;
  trashed_bytes: number;
  failed: { path: string; reason: string }[];
  /** 这次移到废纸篓的条目和它们在废纸篓里的位置 */
  trashed: Trashed[];
  /** 清理做完了、但有别的事没办成时给用户看的提示 */
  warning: string | null;
}

/** 移到废纸篓的一项：原来在哪（from）、现在在废纸篓的哪里（to） */
export interface Trashed {
  from: string;
  to: string;
  size_bytes: number;
}

/** 卸载进度：正在处理第几个应用；waiting 为 true 时在等它退出 */
export interface UninstallProgress {
  name: string;
  done: number;
  total: number;
  waiting: boolean;
}

/** 清理进度：处理到第几项；admin 为 true 时在等用户输管理员密码 */
export interface CleanProgress {
  done: number;
  total: number;
  admin: boolean;
}

export type AiErrorKind = 'consent_required' | 'no_backend' | 'not_logged_in' | 'timeout' | 'bad_output' | 'failed' | 'cancelled';

export interface AiError {
  kind: AiErrorKind;
  message?: string;
}

export interface Answer {
  text: string;
  cached: boolean;
  backend: AiBackend;
}

export type AiBackend = 'claude_cli' | 'api_key';
/** 推理强度，与 Claude Code 的取值一致 */
export type Effort = 'low' | 'medium' | 'high' | 'xhigh' | 'max';
/** Claude Code 自己报出的一个模型。value 为 null 表示它的默认模型，name 是默认实际指向的模型名 */
export interface ClaudeModel {
  value: string | null;
  name: string;
}

/** 「一句话清理」的方案：只可能是扫描报告里已有的类别。 */
export interface Plan {
  categories: CategoryId[];
  reason: string;
  cached: boolean;
  backend: AiBackend;
}

/** 扫描首页的「近 7 天」，全在本机算 */
export interface WeekReport {
  journal: JournalSummary;
  /** 启动盘这 7 天多用了多少（负数是少用）；记录不足两天时为 null */
  disk_delta_bytes: number | null;
}

export interface JournalSummary {
  cleaned_bytes: number;
  uninstalled: number;
  optimized: number;
}

export type AdminTask = 'purge_memory' | 'flush_dns' | 'rebuild_spotlight' | 'thin_snapshots';

/** 免密授权：开没开，以及开了之后哪些任务不再问密码。 */
export interface Passwordless {
  on: boolean;
  tasks: AdminTask[];
}

export interface AdminError {
  kind: 'cancelled' | 'failed';
  message?: string;
}

export interface Leftover {
  path: string;
  /** 还没量过时为 null */
  size_bytes: number | null;
}

/** 认得是这个应用的、但卸载时不动的东西 */
export interface Kept {
  path: string;
  /** 和哪个仍装着的应用共用；为 null 表示要管理员权限才能删，本应用不删 */
  shared_with: string | null;
}

export interface AppInfo {
  /** 应用编号。卸载时只传它。 */
  id: string;
  name: string;
  bundle_id: string;
  version: string;
  path: string;
  size_bytes: number;
  last_used_ms: number | null;
  /** 应用包的创建时间，大致是装上的时间 */
  created_ms: number | null;
  leftovers: Leftover[];
  /** 残留文件的总大小；还没量过时为 null */
  leftover_bytes: number | null;
  /** false 表示限时内没量完，leftover_bytes 只是下限 */
  leftover_complete: boolean;
  /** 卸载时保留、只列出来的 */
  kept: Kept[];
  running: boolean;
  /** 主程序之外、包里正在运行的进程名。卸载前也会请它们退出 */
  tools_running: string[];
}

export interface LoginItem {
  name: string;
  /** daemon：系统级后台服务（/Library/LaunchDaemons），只能看 */
  kind: 'login_item' | 'agent' | 'daemon';
  enabled: boolean;
  /** 全局启动代理：能开关，不能移除 */
  global: boolean;
  path: string;
  /** 启动代理、系统服务要运行的程序；判断不了时为空 */
  program: string | null;
  /** 程序（登录应用就是应用本身）已经不存在 */
  broken: boolean;
  /** 启动代理、系统服务属于哪个应用，只用来显示名字；认不出时为空 */
  app: string | null;
}

export interface LoginItems {
  items: LoginItem[];
  /** 读不到「登录时打开的应用」（没允许控制「系统事件」或它没回应），这一组没列出 */
  apps_unreadable: boolean;
}

export type DockMode = 'show' | 'hide';

export interface MenubarShow {
  cpu: boolean;
  mem: boolean;
  disk: boolean;
}

export interface SettingsView {
  autostart: boolean;
  ai_consent: boolean;
  dock: DockMode;
  ai_backends: AiBackend[];
  /** 用户指定的后端；null 表示自动 */
  ai_backend: AiBackend | null;
  /** 调用 Claude Code 时用的模型和推理强度；null 表示用它自己的默认值 */
  claude: { model: string | null; effort: Effort | null };
  /** 自填的 AI 服务。密钥不会返回给界面，只知道有没有 */
  api: { base_url: string; model: string } | null;
  api_key_set: boolean;
  onboarded: boolean;
  low_disk_alert: boolean;
  /** 菜单栏图标旁边用文字显示哪几项 */
  menubar: MenubarShow;
  full_disk_access: boolean;
  version: string;
  /** 用户选的界面语言；null 表示跟随系统 */
  language: Lang | null;
  /** 实际在用的界面语言 */
  lang: Lang;
}

/** AI 给「旧下载文件」分的组。只是建议。 */
export interface Organized {
  groups: { title: string; advice: 'delete' | 'keep' | 'review'; why: string; item_ids: string[]; total_bytes: number }[];
  cached: boolean;
  backend: AiBackend;
}

/** 空间去向的一个节点。path 是显示用路径（已去掉数据卷前缀）。size_bytes = 子项之和 + other_bytes */
export interface SpaceNode {
  name: string;
  path: string;
  size_bytes: number;
  is_dir: boolean;
  /** 落在已有清理类别里，显示「去清理」 */
  cleanable: boolean;
  /** 个人文件夹里的个人文件，可以移到废纸篓 */
  trashable: boolean;
  /** 按大小降序 */
  children: SpaceNode[];
  other_bytes: number;
}

export interface SpaceReport {
  root: SpaceNode;
  unreadable: string[];
  unreadable_count: number;
  purgeable_bytes: number;
  /** 中途取消时为 false，数字偏小 */
  finished: boolean;
  files: number;
  /** 统计完成的时间（毫秒时间戳）；结果存在磁盘上，重启后还在 */
  scanned_at_ms: number;
}

/** 统计中的一个位置：个人文件夹下的一级（如 ~/Library），或个人文件夹以外的一级（如 /Applications） */
export interface SpaceTop {
  path: string;
  /** 没数完时是已经数到的大小 */
  bytes: number;
  done: boolean;
}

export interface SpaceProgress {
  files: number;
  bytes: number;
  /** 目前最大的几个位置，按大小降序 */
  tops: SpaceTop[];
}

export interface Usage {
  size_bytes: number;
  reclaimable_bytes: number;
  count: number;
}

export type DockerState =
  | { state: 'not_installed' }
  | { state: 'not_running' }
  | { state: 'ready'; images: Usage; containers: Usage; volumes: Usage; build_cache: Usage };

export interface SimDevice {
  udid: string;
  name: string;
  runtime: string;
  /** 例如 iOS 18.3 */
  os: string;
  available: boolean;
  size_bytes: number;
  last_used_ms: number | null;
}

export interface SimRuntime {
  id: string;
  runtime: string;
  os: string;
  version: string;
  size_bytes: number;
  deletable: boolean;
  in_use: boolean;
}

export interface DevEnvReport {
  docker: DockerState;
  /** 没装 Xcode 时为空 */
  simulators: { devices: SimDevice[]; runtimes: SimRuntime[] } | null;
}

/** 和后端 remove::Target 一一对应 */
export type DevTarget =
  | { kind: 'docker_build_cache' }
  | { kind: 'docker_stopped_containers' }
  | { kind: 'docker_unused_images' }
  | { kind: 'sim_unavailable' }
  | { kind: 'sim_device'; id: string }
  | { kind: 'sim_runtime'; id: string };

export interface UpdateInfo {
  version: string;
  notes: string | null;
}

/** 和后端 db::UsageTrend 一一对应。day 是自 1970 年起的第几天（UTC） */
export interface UsageTrend {
  points: { day: number; used_bytes: number; total_bytes: number }[];
  /** 每天多用（负数是少用）多少字节 */
  bytes_per_day: number;
  /** 用量在涨时，照这个速度还有几天写满 */
  days_to_full: number | null;
}
