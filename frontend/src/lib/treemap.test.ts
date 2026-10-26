import { test } from 'node:test';
import assert from 'node:assert/strict';
import { squarify } from './treemap.ts';

test('squarify 铺满矩形、面积成比例、不越界不重叠', () => {
  const values = [432, 143, 65, 18, 8, 7, 6, 2];
  const box = { x: 0, y: 0, w: 300, h: 100 };
  const rs = squarify(values, box);
  const total = values.reduce((a, b) => a + b, 0);
  const eps = 1e-6;
  rs.forEach((r, i) => {
    assert.ok(Math.abs(r.w * r.h - (values[i] / total) * 30000) < eps, `面积 ${i}`);
    assert.ok(r.x >= -eps && r.y >= -eps && r.x + r.w <= 300 + eps && r.y + r.h <= 100 + eps, `越界 ${i}`);
  });
  for (let i = 0; i < rs.length; i++)
    for (let j = i + 1; j < rs.length; j++) {
      const [a, b] = [rs[i], rs[j]];
      const overlap = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x) > eps && Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y) > eps;
      assert.ok(!overlap, `${i} 和 ${j} 重叠`);
    }
});

test('squarify 论文里的例子：6×4 里铺 6 6 4 3 2 2 1，每块长宽比都不超过 3', () => {
  const rs = squarify([6, 6, 4, 3, 2, 2, 1], { x: 0, y: 0, w: 6, h: 4 });
  for (const r of rs) assert.ok(Math.max(r.w / r.h, r.h / r.w) <= 3);
});
