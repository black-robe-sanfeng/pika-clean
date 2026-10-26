import { type ReactNode, useEffect, useRef } from 'react';

/**
 * 对话框。按 Esc 等于点「取消」：按钮栏里最后一个可点的灰色按钮（本应用的对话框都把取消、稍后做成灰色按钮）；
 * 没有灰色按钮的（欢迎、结果）按 Esc 不关，正在执行时按钮是禁用的，按 Esc 也不关。
 * 按 Tab 只在对话框里转，不会跳到被遮住的页面上。
 * 打开时焦点放到「取消」上，没有就放到第一个不是红色的按钮上，不会默认落在删除这类按钮上；里面的输入框自己抢了焦点时不动。
 */
export function Dialog(props: { title: string; children: ReactNode; actions: ReactNode }) {
  const box = useRef<HTMLDivElement>(null);
  const cancel = () => [...(box.current?.querySelectorAll<HTMLButtonElement>('.acts .btn.gray:not(:disabled)') ?? [])].pop();
  useEffect(() => {
    if (box.current?.contains(document.activeElement)) return;
    (cancel() ?? box.current?.querySelector<HTMLButtonElement>('.acts button:not(.red)'))?.focus();
  }, []);
  return (
    <div
      className="scrim"
      role="dialog"
      aria-modal="true"
      aria-label={props.title}
      onKeyDown={(e) => {
        if (e.key === 'Tab') {
          const all = [...(box.current?.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea, a[href], [tabindex]:not([tabindex="-1"])') ?? [])];
          const [first, last] = [all[0], all[all.length - 1]];
          if (!first) return e.preventDefault();
          if (e.shiftKey ? document.activeElement === first || !box.current?.contains(document.activeElement) : document.activeElement === last) {
            e.preventDefault();
            (e.shiftKey ? last : first).focus();
          }
          return;
        }
        if (e.key !== 'Escape') return;
        // 只关这一层：设置页的下拉菜单也在窗口上听 Esc
        e.stopPropagation();
        cancel()?.click();
      }}
    >
      <div className="dialog" ref={box}>
        <h2>{props.title}</h2>
        {props.children}
        <div className="acts">{props.actions}</div>
      </div>
    </div>
  );
}
