export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** 把一组按降序排好的正数铺满 box，面积和数值成正比，尽量铺成方块（squarified 算法）。返回的矩形和 values 一一对应 */
export function squarify(values: number[], box: Rect): Rect[] {
  const total = values.reduce((a, b) => a + b, 0);
  const areas = values.map((v) => (v * box.w * box.h) / total);
  const out: Rect[] = [];
  let { x, y, w, h } = box;
  let i = 0;
  while (i < areas.length) {
    const side = Math.min(w, h);
    // 一行里最扁那块的长宽比；areas 降序，所以最大是第一块、最小是最后一块
    const worst = (sum: number, last: number) => Math.max((side * side * areas[i]) / (sum * sum), (sum * sum) / (side * side * areas[last]));
    let sum = areas[i];
    let j = i + 1;
    // 往这一行继续加，直到再加会让它更扁
    while (j < areas.length && worst(sum + areas[j], j) <= worst(sum, j - 1)) sum += areas[j++];
    const thick = sum / side;
    let off = 0;
    for (let k = i; k < j; k++) {
      const len = areas[k] / thick;
      out.push(w >= h ? { x, y: y + off, w: thick, h: len } : { x: x + off, y, w: len, h: thick });
      off += len;
    }
    if (w >= h) {
      x += thick;
      w -= thick;
    } else {
      y += thick;
      h -= thick;
    }
    i = j;
  }
  return out;
}
