import type { Category, CategoryId } from '../api/types';

/** a 就是 b，或在 b 里面 */
const within = (a: string, b: string) => a === b || a.startsWith(b.endsWith('/') ? b : `${b}/`);

/** 空间去向里点「去清理」的位置属于哪个清理类别：类别里有条目在这个位置之下，或这个位置就在某个条目里面。 */
export function categoryAt(categories: Category[], path: string): CategoryId | undefined {
  return categories.find((c) => c.items.some((i) => within(i.path, path) || within(path, i.path)))?.id;
}

/** Docker、模拟器的目录在清理页「开发环境」那一组里清（规格切片 17）。 */
export function devGroupAt(path: string): 'docker' | 'sims' | null {
  if (/\/Library\/Containers\/com\.docker\.docker(\/|$)/.test(path)) return 'docker';
  if (/\/Library\/Developer\/CoreSimulator(\/|$)/.test(path)) return 'sims';
  return null;
}
