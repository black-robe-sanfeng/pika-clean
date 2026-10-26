/** 圆形勾选框。some 表示只选了其中一部分，显示成一道横线。 */
export function Check({ all, some = false, ...rest }: { all: boolean; some?: boolean; disabled?: boolean; onChange: () => void; 'aria-label'?: string }) {
  return (
    <input
      className="ck"
      type="checkbox"
      checked={all}
      ref={(el) => {
        if (el) el.indeterminate = some && !all;
      }}
      {...rest}
    />
  );
}
