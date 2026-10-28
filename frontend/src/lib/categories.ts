import type { Category, CategoryId } from '../api/types';
import type { IconName } from '../components/Icon';
import { t } from './i18n.ts';

/** 各类别在界面上的名称和图标。类别本身由后端的扫描规则定义。 */
export const CATEGORY: Record<CategoryId, { name: string; desc: string; icon: IconName; color: string }> = {
  app_cache: { name: t('应用缓存'), desc: t('第三方应用的缓存，用到时会重新生成'), icon: 'box', color: '' },
  system_cache: { name: t('系统缓存'), desc: t('所有用户共用的缓存'), icon: 'chip', color: 'c-gray' },
  trash: { name: t('废纸篓'), desc: t('已删除但仍占用空间的文件'), icon: 'trash', color: 'c-red' },
  old_downloads: { name: t('旧下载文件'), desc: t('超过 90 天没有打开过，建议先查看'), icon: 'down', color: 'c-orange' },
  logs: { name: t('日志文件'), desc: t('系统与应用的运行日志'), icon: 'doc', color: 'c-teal' },
  stale_projects: { name: t('不活跃项目的依赖'), desc: t('超过 90 天没动过的项目里的 node_modules、target 等，需要时可重新安装'), icon: 'cube', color: 'c-orange' },
  dev_cache: { name: t('开发工具缓存'), desc: t('包管理器下载的依赖、Xcode 构建缓存和模拟器缓存，删了下次构建会变慢'), icon: 'cube', color: 'c-purple' },
  ai_cache: { name: t('AI 工具缓存'), desc: t('Claude、Codex、OpenCode、Cursor 等的缓存和日志，清理前先退出这些工具'), icon: 'scan', color: 'c-indigo' },
  ai_history: { name: t('AI 对话记录'), desc: t('超过 30 天没动过的会话，删了就不能再接着聊；不含 Claude Code 的长期记忆'), icon: 'chat', color: 'c-teal' },
  crash_dumps: { name: t('系统崩溃转储'), desc: t('程序崩溃时留下的内存转储，只在排查崩溃时有用'), icon: 'doc', color: 'c-red' },
  ios_backups: { name: t('iOS 设备备份'), desc: t('iPhone、iPad 在这台 Mac 上的备份，可能是设备数据仅有的一份'), icon: 'drive', color: 'c-gray' },
  xcode_archives: { name: t('Xcode 归档'), desc: t('打包上架时留下的归档，里面有还原崩溃日志要用的符号文件'), icon: 'box', color: 'c-purple' },
  ios_firmware: { name: t('iOS 固件'), desc: t('给 iPhone、iPad 更新或刷机时下载的系统固件，需要时可重新下载'), icon: 'down', color: 'c-indigo' },
  mail_downloads: { name: t('邮件附件'), desc: t('「邮件」打开附件时存下的副本，原件仍在邮件里'), icon: 'doc', color: 'c-teal' },
  homebrew_old: { name: t('Homebrew 旧版本'), desc: t('升级后留下的旧版本，只列出 brew cleanup 认为可以删的'), icon: 'cube', color: 'c-orange' },
  duplicates: { name: t('重复文件'), desc: t('下载、桌面、文稿里内容完全相同的文件，每组留一份，其余列在这里'), icon: 'doc', color: 'c-orange' },
  browser_privacy: { name: t('浏览器记录和 Cookie'), desc: t('Safari、Chrome、Edge、Brave 的浏览记录和 Cookie，删了会退出各网站的登录；书签和密码不动'), icon: 'chat', color: 'c-indigo' },
  large_files: { name: t('大文件和旧文件'), desc: t('个人文件夹里超过 50 MB，或一年没打开且超过 10 MB 的文件'), icon: 'doc', color: 'c-red' },
};

/** 这一类现在能不能清理。读不到的、空的不能。需要管理员的可以，清理时系统会弹密码框。 */
export const cleanable = (c: Category) => !c.unreadable && c.total_bytes > 0;

export const cleanableBytes = (cats: Category[]) => cats.filter(cleanable).reduce((sum, c) => sum + c.total_bytes, 0);

export const REMOVAL = { permanent: t('直接删除'), trash: t('移到废纸篓') } as const;
