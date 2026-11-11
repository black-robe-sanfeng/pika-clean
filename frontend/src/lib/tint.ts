const TINTS = ['', 'c-green', 'c-orange', 'c-red', 'c-purple', 'c-teal', 'c-indigo', 'c-gray'];

/** 同一个名字总是同一种颜色，让列表里的应用和进程一眼能区分。是应用时 `AppIcon` 会换成真实图标。 */
export const tint = (name: string) => TINTS[[...name].reduce((h, ch) => (h * 31 + ch.charCodeAt(0)) >>> 0, 7) % TINTS.length];
