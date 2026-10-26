import type { ReactNode } from 'react';
import { t } from '../lib/i18n.ts';

/** 主窗口右侧滑出的面板。条目明细和 AI 回答共用这一个。 */
export function Sheet(props: { open: boolean; icon?: ReactNode; title: string; onClose: () => void; footer?: ReactNode; children: ReactNode }) {
  return (
    // 关着的时候设成 inert：看不见的控件不能再被 Tab 选中；开着时按 Esc 关闭
    <aside
      className={`sheet${props.open ? ' on' : ''}`}
      aria-hidden={!props.open}
      inert={!props.open}
      onKeyDown={(e) => {
        if (e.key !== 'Escape') return;
        e.stopPropagation();
        props.onClose();
      }}
    >
      <div className="sh-hd">
        {props.icon}
        <b>{props.title}</b>
        <button onClick={props.onClose} aria-label={t('关闭')}>
          ×
        </button>
      </div>
      <div className="sh-b">{props.children}</div>
      {props.footer && <div className="sh-f">{props.footer}</div>}
    </aside>
  );
}
