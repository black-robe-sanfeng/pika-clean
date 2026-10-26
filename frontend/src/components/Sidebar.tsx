import { type IconName, Tile } from './Icon';
import { t } from '../lib/i18n.ts';

export const VIEWS = ['scan', 'space', 'opt', 'care', 'apps', 'settings'] as const;
export type ViewId = (typeof VIEWS)[number];

export type NavItem = { id: ViewId; label: string; icon: IconName; color: string };

// 清理的各个类别只在智能扫描的结果里列一次，侧栏不再按类别分页（用户 2026-10-15、10-16 两次确认：分类页是智能扫描的子类，多余）
export const GROUPS: { title?: string; items: NavItem[] }[] = [
  { items: [{ id: 'scan', label: t('智能扫描'), icon: 'scan', color: '' }, { id: 'space', label: t('空间去向'), icon: 'drive', color: 'c-teal' }] },
  { title: t('速度'), items: [{ id: 'opt', label: t('优化速度'), icon: 'bolt', color: 'c-purple' }, { id: 'care', label: t('保养 Mac'), icon: 'batt', color: 'c-green' }] },
  { title: t('应用程序'), items: [{ id: 'apps', label: t('卸载软件'), icon: 'app', color: 'c-orange' }] },
];

export function Sidebar({ view, onGo }: { view: ViewId; onGo: (v: ViewId) => void }) {
  return (
    <aside className="side">
      <div className="brand">
        <Tile name="logo" />
        Pika Clean
      </div>
      <nav className="nav">
        {GROUPS.map((g) => (
          <div key={g.items[0].id}>
            {g.title && <div className="nav-h">{g.title}</div>}
            {g.items.map((n) => (
              <button key={n.id} className={view === n.id ? 'on' : ''} aria-current={view === n.id ? 'page' : undefined} onClick={() => onGo(n.id)}>
                <Tile name={n.icon} color={n.color} />
                {n.label}
              </button>
            ))}
          </div>
        ))}
      </nav>
      <nav className="nav foot">
        <button className={view === 'settings' ? 'on' : ''} aria-current={view === 'settings' ? 'page' : undefined} onClick={() => onGo('settings')}>
          <Tile name="gear" color="c-gray" />
          {t('设置')}
        </button>
      </nav>
    </aside>
  );
}
