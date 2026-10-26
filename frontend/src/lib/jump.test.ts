import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Category } from '../api/types';
import { categoryAt, devGroupAt } from './jump.ts';

test('去清理：按位置找到所属的清理类别，或开发环境那一组', () => {
  const cat = (id: string, ...paths: string[]) => ({ id, items: paths.map((path, i) => ({ id: `${id}${i}`, path, size_bytes: 1, last_used_ms: null })) }) as unknown as Category;
  const cats = [cat('logs', '/Users/me/Library/Logs/a.log'), cat('crash_dumps', '/cores/core.1'), cat('app_cache', '/Users/me/Library/Caches/com.x')];
  assert.equal(categoryAt(cats, '/cores'), 'crash_dumps', '条目在这个位置之下');
  assert.equal(categoryAt(cats, '/Users/me/Library/Caches/com.x/sub'), 'app_cache', '位置在条目里面');
  assert.equal(categoryAt(cats, '/Users/me/Library/Cache'), undefined, '名字前缀相同不算');
  assert.equal(devGroupAt('/Users/me/Library/Containers/com.docker.docker/Data'), 'docker');
  assert.equal(devGroupAt('/Users/me/Library/Developer/CoreSimulator'), 'sims');
  assert.equal(devGroupAt('/Users/me/Library/Developer/Xcode'), null);
});
