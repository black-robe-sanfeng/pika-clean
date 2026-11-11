import { useEffect, useState } from 'react';
import { api } from '../api';
import { tint } from '../lib/tint';

// 同一个应用在一个窗口里只向后端要一次；done 存已取到的结果，列表重新挂载时不闪字母
const pending = new Map<string, Promise<string | null>>();
const done = new Map<string, string | null>();

function load(path: string) {
  let p = pending.get(path);
  if (!p) {
    p = api
      .appIcon(path)
      .then((b) => (b.byteLength ? URL.createObjectURL(new Blob([b], { type: 'image/png' })) : null))
      .catch(() => null)
      .then((src) => (done.set(path, src), src));
    pending.set(path, p);
  }
  return p;
}

/** 应用自己的图标；不是应用或取不到时，显示名字首字母的彩色方块。 */
export function AppIcon({ name, path, size = '' }: { name: string; path?: string | null; size?: '' | 'sm' | 'lg' }) {
  const [src, setSrc] = useState(() => (path && done.get(path)) || null);
  useEffect(() => {
    if (!path) return setSrc(null);
    let alive = true;
    load(path).then((s) => alive && setSrc(s));
    return () => {
      alive = false;
    };
  }, [path]);
  return src ? (
    <span className={`ic ${size} app-icon`} aria-hidden="true">
      <img src={src} alt="" />
    </span>
  ) : (
    <span className={`ic ${size} ${tint(name)}`} aria-hidden="true">
      {name.charAt(0).toUpperCase()}
    </span>
  );
}
