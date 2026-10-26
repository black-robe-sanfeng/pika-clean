// 界面文案的翻译。源码里直接写简体中文，它同时就是查词典用的键；
// 所选语言的词典里没有这一句时原样显示中文，所以漏翻不会出现空白或报错。

export type Lang = 'zh-Hans' | 'zh-Hant' | 'en' | 'ja' | 'ko';

/** 语言各自的名字，不随界面语言变。顺序就是设置里的顺序。 */
export const LANGS: [Lang, string][] = [
  ['zh-Hans', '简体中文'],
  ['zh-Hant', '繁體中文'],
  ['en', 'English'],
  ['ja', '日本語'],
  ['ko', '한국어'],
];

/** 词典按需加载：只加载所选语言的那一份，简体中文一份都不用。 */
const DICTS: Record<Exclude<Lang, 'zh-Hans'>, () => Promise<Record<string, string>>> = {
  'zh-Hant': () => import('../i18n/zh-Hant.ts').then((m) => m.zhHant),
  en: () => import('../i18n/en.ts').then((m) => m.en),
  ja: () => import('../i18n/ja.ts').then((m) => m.ja),
  ko: () => import('../i18n/ko.ts').then((m) => m.ko),
};

let dict: Record<string, string> | undefined;

/** 切换界面语言。要在界面模块加载之前调用并等它完成：不少文案是模块顶层的常量，加载时就取了译文。 */
export async function setLang(lang: Lang) {
  dict = lang === 'zh-Hans' ? undefined : await DICTS[lang]();
}

/** 翻译一句文案。`{0}` `{1}` 依次换成后面的参数，译文里可以调换它们的位置。 */
export function t(zh: string, ...args: (string | number)[]): string {
  const s = dict?.[zh] ?? zh;
  return args.length ? s.replace(/\{(\d+)\}/g, (_, i) => String(args[Number(i)])) : s;
}
