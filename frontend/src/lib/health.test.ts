import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Snapshot } from '../api/types.ts';
import { health } from './health.ts';

const base: Snapshot = {
  ts_ms: 0,
  uptime_s: 0,
  cpu: { usage_pct: 10, load1: 2, cores: 8, temp_c: null },
  gpu: null,
  mem: { used_bytes: 8, total_bytes: 16, pressure: 'normal', swap_used_bytes: 0, swap_total_bytes: 0 },
  disk: { used_bytes: 50, total_bytes: 100, health_pct: null },
  net: { down_bps: 0, up_bps: 0, iface: '' },
  battery: null,
  fan: null,
  mic_in_use: null,
  camera_in_use: null,
  top: [],
  cpu_hist: [],
  net_hist: [],
};

test('一切正常时满分', () => {
  assert.deepEqual(health(base), { score: 100, label: '良好', reasons: [] });
});

test('各项扣分和分档', () => {
  const full = health({ ...base, disk: { used_bytes: 95, total_bytes: 100, health_pct: null } });
  assert.deepEqual([full.score, full.label, full.reasons], [70, '一般', ['磁盘已用 95%']]);

  const bad = health({
    ...base,
    disk: { used_bytes: 100, total_bytes: 100, health_pct: null },
    mem: { ...base.mem, pressure: 'critical' },
    cpu: { ...base.cpu, load1: 40 },
    battery: { pct: 50, charging: false, health_pct: 70, cycles: 900 },
  });
  assert.equal(bad.score, 0);
  assert.equal(bad.label, '需要关注');
  assert.equal(bad.reasons.length, 4);

  assert.equal(health({ ...base, mem: { ...base.mem, pressure: 'warn' } }).score, 85);
});
