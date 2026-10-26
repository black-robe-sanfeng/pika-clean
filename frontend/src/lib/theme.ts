/** 主题同时作用于主窗口和菜单栏面板；原皮肤下面板仍是系统玻璃。 */
export const THEMES = [
  { id: 'original', name: '原皮肤', color: '#3567D6', paper: '#3567D6' },
  { id: 'jade', name: '青绿山水', color: '#216f62', paper: '#f4f2e7' },
  { id: 'ink', name: '水墨竹影', color: '#526773', paper: '#f1f3f2' },
  { id: 'plum', name: '朱砂梅韵', color: '#a74935', paper: '#f8efdf' },
  { id: 'ember', name: '玄金祥云', color: '#c45a37', paper: '#191815' },
  { id: 'lotus', name: '敦煌莲华', color: '#08777b', paper: '#fff3da' },
  { id: 'porcelain', name: '青花瓷', color: '#24569a', paper: '#f5f6f3' },
] as const;
export type ThemeId = (typeof THEMES)[number]['id'];

export function resolveTheme(value: string | null): ThemeId {
  return THEMES.find(({ id }) => id === value)?.id ?? 'ink';
}

export function currentTheme(): ThemeId {
  try {
    return resolveTheme(localStorage.getItem('theme'));
  } catch {
    return 'ink';
  }
}

export function applyTheme(id: ThemeId) {
  document.documentElement.dataset.theme = id;
  // 旧版本的自定义主色不再覆盖主题的配色。
  document.documentElement.style.removeProperty('--accent');
}

export function setTheme(id: ThemeId) {
  // 保存失败时由调用方提示，避免显示成功却在重启后丢失。
  localStorage.setItem('theme', id);
  applyTheme(id);
}
