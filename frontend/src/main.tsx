import './styles.css';
import { boot } from './app/boot';
import { applyTheme, currentTheme } from './lib/theme';

applyTheme(currentTheme());

boot(async () => {
  const { App } = await import('./app/App');
  return <App />;
});
