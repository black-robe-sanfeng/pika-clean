import { test } from 'node:test';
import assert from 'node:assert/strict';
import { ago, bytes, mem, pct, rate, uptime } from './fmt.ts';

test('bytes 和 Finder 一样按 1000 进位', () => {
  // df 报的 235,782,885,376 字节：Finder 写 236 GB，按 1024 算就成了 220
  assert.equal(bytes(235_782_885_376), '236 GB');
  assert.equal(bytes(1.5e9), '1.5 GB');
  assert.equal(bytes(320e6), '320 MB');
  assert.equal(bytes(999e6), '999 MB');
  assert.equal(bytes(12e3), '12 KB');
});

test('mem 和活动监视器一样按 1024 进位', () => {
  assert.equal(mem(1.5 * 1024 ** 3), '1.5 GB');
  assert.equal(mem(1012 * 1024 ** 2), '1.0 GB');
  assert.equal(mem(999 * 1024 ** 2), '999 MB');
  assert.equal(mem(12 * 1024), '12 KB');
});

test('pct 在总量为 0 时不产生 NaN', () => {
  assert.equal(pct(1, 0), 0);
  assert.equal(pct(408, 494), 83);
});

test('rate 自动切换单位', () => {
  assert.deepEqual(rate(2.4 * 1024 ** 2), ['2.4', 'MB/s']);
  assert.deepEqual(rate(163 * 1024), ['163', 'KB/s']);
});

test('uptime 与 ago 的分档', () => {
  assert.equal(uptime(9 * 86400 + 5), '已开机 9 天');
  assert.equal(uptime(3 * 3600), '已开机 3 小时');
  const now = Date.UTC(2026, 9, 13);
  assert.equal(ago(now - 91 * 86400000, now), '3 个月前');
  assert.equal(ago(null), '未知');
});
