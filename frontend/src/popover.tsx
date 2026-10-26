import './styles.css';
import { boot } from './app/boot';
import { applyTheme, currentTheme } from './lib/theme';

// 面板窗口启动时只建一次，之后只是显示/隐藏；每次弹出（获得焦点）重读一遍，跟上设置里换的主题。
applyTheme(currentTheme());
addEventListener('focus', () => applyTheme(currentTheme()));

boot(async () => {
  const { PopoverPage } = await import('./pages/PopoverPage');
  return <PopoverPage />;
});
