import appLogo from '../../../backend/app-icon.svg';

const ICONS = {
  scan: '<path d="M11 4l1.7 4.8L17.5 10.5l-4.8 1.7L11 17l-1.7-4.8L4.5 10.5l4.8-1.7zM18.5 15.5v4M16.5 17.5h4" fill="currentColor"/>',
  trash: '<path d="M4 7h16M10 4h4M6 7l1 13h10l1-13M10 11v5M14 11v5"/>',
  app: '<rect x="4" y="4" width="16" height="16" rx="4"/><path d="M9.5 9.5l5 5M14.5 9.5l-5 5"/>',
  bolt: '<path d="M13 3L6 13.5h5L10 21l8-11h-5.5z" fill="currentColor"/>',
  box: '<path d="M4 8h16v11H4zM3 5h18v3H3zM10 12h4"/>',
  chip: '<rect x="7" y="7" width="10" height="10" rx="2"/><path d="M3.5 10H7M3.5 14H7M17 10h3.5M17 14h3.5M10 3.5V7M14 3.5V7M10 17v3.5M14 17v3.5"/>',
  down: '<path d="M12 4v11M7 11l5 5 5-5M5 20h14"/>',
  doc: '<path d="M7 3h7l4 4v14H7zM14 3v4h4M10 12h5M10 16h5"/>',
  cube: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9zM12 12l8-4.5M12 12v9M12 12L4 7.5"/>',
  globe: '<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c3.2 3 3.2 15 0 18M12 3c-3.2 3-3.2 15 0 18"/>',
  drive: '<rect x="3.5" y="6" width="17" height="12" rx="3"/><path d="M7 14.5h.01M11 14.5h6"/>',
  chev: '<path d="M9 6l6 6-6 6"/>',
  gpu: '<path d="M4 5h16v11H4zM9 20h6M12 16v4"/>',
  mem: '<path d="M4 8h16v8H4zM8 16v3M12 16v3M16 16v3M8 11v2M12 11v2M16 11v2"/>',
  batt: '<path d="M3 8h15v8H3zM21 11v2M6 11v2M9 11v2"/>',
  bars: '<path d="M5 20V10M10 20V4M15 20v-8M20 20v-5"/>',
  fan: '<circle cx="12" cy="12" r="1.5"/><path d="M12 10.5c-1-3.5 0-6.5 2.5-6.5 2.2 0 3 2.3 1.8 4-1 1.4-2.5 2.2-4.3 2.5zM13.3 12.8c3.5.9 5.6 3.2 4.4 5.4-1.1 1.9-3.5 1.4-4.4-.5-.7-1.6-.6-3.3 0-4.9zM10.7 12.8c-2.5 2.6-5.6 3.3-6.9 1.1-1.1-1.9.5-3.7 2.6-3.5 1.7.2 3.2 1 4.3 2.4z"/>',
  cam: '<path d="M4 8h3l1.5-2h7L17 8h3v10H4z"/><circle cx="12" cy="13" r="3"/>',
  mic: '<rect x="9.5" y="4" width="5" height="10" rx="2.5"/><path d="M6.5 11.5a5.5 5.5 0 0011 0M12 17v3"/>',
  awake: '<path d="M5 9h11v5a4 4 0 01-4 4H9a4 4 0 01-4-4zM16 10h2a2 2 0 010 4h-2M8 3v3M12 3v3"/>',
  eject: '<path d="M12 5l7 8H5zM5 18h14"/>',
  chat: '<path d="M5 5h14v10H10l-4 4v-4H5z"/><path d="M9 9.5h6M9 12h4"/>',
  check: '<path d="M5 12.5l4.5 4.5L19 7.5"/>',
  gear: '<path d="M4 7h10M18 7h2M4 17h2M10 17h10"/><circle cx="16" cy="7" r="2"/><circle cx="8" cy="17" r="2"/>',
} as const;

export type IconName = keyof typeof ICONS | 'logo';

export function Icon({ name, className }: { name: IconName; className?: string }) {
  if (name === 'logo') return <img src={appLogo} className={className} alt="" aria-hidden="true" />;
  return <svg viewBox="0 0 24 24" className={className} aria-hidden="true" dangerouslySetInnerHTML={{ __html: ICONS[name] }} />;
}

/** 彩色圆角图标底板，颜色类名见 styles.css 的 .c-*。 */
export function Tile({ name, color = '', size = '' }: { name: IconName; color?: string; size?: 'lg' | 'sm' | '' }) {
  return (
    <span className={`ic ${color} ${size}${name === 'logo' ? ' brand-icon' : ''}`}>
      <Icon name={name} />
    </span>
  );
}
