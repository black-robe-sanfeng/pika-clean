import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { setLang, t } from './i18n.ts';
import { en } from '../i18n/en.ts';
import { ja } from '../i18n/ja.ts';
import { ko } from '../i18n/ko.ts';
import { zhHant } from '../i18n/zh-Hant.ts';

const DICTS = { 'zh-Hant': zhHant, en, ja, ko };

const SRC = join(import.meta.dirname, '..');

/** 源码里每一处 t('…') 的原文。 */
function sourceKeys(): Set<string> {
  const keys = new Set<string>();
  for (const name of readdirSync(SRC, { recursive: true, encoding: 'utf8' })) {
    if (!/\.tsx?$/.test(name) || /(^|\/)i18n|\.test\.ts$|mock\.ts$/.test(name)) continue;
    for (const m of readFileSync(join(SRC, name), 'utf8').matchAll(/\bt\('((?:[^'\\]|\\.)*)'/g)) {
      keys.add(m[1].replace(/\\(['\\])/g, '$1').replace(/\\n/g, '\n'));
    }
  }
  return keys;
}

const placeholders = (s: string) => (s.match(/\{\d+\}/g) ?? []).sort().join();

test('每种语言的词典都覆盖源码里的全部文案，占位符和原文一致', () => {
  const keys = sourceKeys();
  assert.ok(keys.size > 300, '没扫到源码里的文案，检查一下扫描逻辑');
  for (const [lang, dict] of Object.entries(DICTS)) {
    const missing = [...keys].filter((k) => !(k in dict));
    assert.deepEqual(missing, [], `${lang} 缺译文`);
    for (const [zh, tr] of Object.entries(dict)) assert.equal(placeholders(tr), placeholders(zh), `${lang} 的占位符不一致：${zh}`);
  }
});

test('按所选语言取译文并填入参数；词典里没有的原样显示中文', async () => {
  assert.equal(t('已选 {0} 类', 3), '已选 3 类');
  await setLang('en');
  assert.equal(t('已选 {0} 类', 3), '3 selected');
  assert.equal(t('已用 {0} / {1}', '1 GB', '2 GB'), '1 GB of 2 GB used');
  assert.equal(t('词典里没有的一句'), '词典里没有的一句');
  await setLang('zh-Hans');
  assert.equal(t('清理'), '清理');
});
