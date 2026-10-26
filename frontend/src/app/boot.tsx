import type { ReactElement } from 'react';
import { createRoot } from 'react-dom/client';
import { api } from '../api';
import { type Lang, setLang } from '../lib/i18n.ts';

/**
 * 两个窗口共用的启动步骤：先问后端界面用哪种语言，再加载界面。
 * 界面模块必须在定下语言之后才加载（所以用动态 import），因为不少文案是模块顶层的常量。
 * 换语言时后端会通知每个窗口，窗口整个重新加载一遍。
 */
export async function boot(load: () => Promise<ReactElement>) {
  // 读不到设置时照常用简体中文启动，不能因为这个白屏
  const lang = await api.settingsGet().then(
    (s) => s.lang,
    () => 'zh-Hans' as Lang,
  );
  // 词典加载失败时照常用简体中文启动
  await setLang(lang).catch(() => {});
  document.documentElement.lang = lang;
  api.onLanguageChange(() => location.reload());
  createRoot(document.getElementById('root')!).render(await load());
}
