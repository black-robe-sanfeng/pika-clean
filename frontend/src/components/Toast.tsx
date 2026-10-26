import { useRef, useSyncExternalStore } from 'react';

let current = '';
let timer = 0;
const listeners = new Set<() => void>();
const notify = () => listeners.forEach((l) => l());

/** 显示一句提示。长的（多半是报错）按字数多留一会儿，最少 2.4 秒、最多 10 秒，来得及读完和选中复制。 */
export function toast(msg: string) {
  current = msg;
  notify();
  clearTimeout(timer);
  timer = window.setTimeout(
    () => {
      current = '';
      notify();
    },
    Math.min(10_000, Math.max(2400, msg.length * 150)),
  );
}

export function ToastHost() {
  const msg = useSyncExternalStore(
    (l) => (listeners.add(l), () => listeners.delete(l)),
    () => current,
  );
  // 淡出期间继续显示上一条文字
  const last = useRef('');
  if (msg) last.current = msg;
  return (
    <div className={`toast${msg ? ' on' : ''}`} role="status">
      {last.current}
    </div>
  );
}
