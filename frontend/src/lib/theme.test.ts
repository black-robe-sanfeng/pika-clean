import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resolveTheme } from './theme.ts';

test('首次使用、旧版主色和损坏的主题值均使用水墨竹影', () => {
  for (const value of [null, '', '#3567D6', 'unknown', 'toString', '__proto__']) {
    assert.equal(resolveTheme(value), 'ink');
  }
});

test('保存的每一种主题均可恢复', () => {
  for (const id of ['original', 'jade', 'ink', 'plum', 'ember', 'lotus', 'porcelain'] as const) {
    assert.equal(resolveTheme(id), id);
  }
});
