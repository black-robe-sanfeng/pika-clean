import { useEffect, useState } from 'react';

/**
 * 「再点一次才执行」：第一次点只是进入待确认状态，几秒内不再点就自动复位。
 * 用在没有地方弹确认框的菜单栏面板里。`key` 用来区分同一组里的多个按钮。
 */
export function useArmed(timeoutMs = 4000) {
  const [armed, setArmed] = useState<string | null>(null);
  useEffect(() => {
    if (armed == null) return;
    const t = setTimeout(() => setArmed(null), timeoutMs);
    return () => clearTimeout(t);
  }, [armed, timeoutMs]);
  /** 返回 true 表示这次点击是确认，应该执行。 */
  const press = (key: string) => {
    if (armed === key) {
      setArmed(null);
      return true;
    }
    setArmed(key);
    return false;
  };
  return { armed, press };
}
