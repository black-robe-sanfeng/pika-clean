const TINTS = ['', 'c-green', 'c-orange', 'c-red', 'c-purple', 'c-teal', 'c-indigo', 'c-gray'];

/** 同一个名字总是同一种颜色，让列表里的应用和进程一眼能区分。真实的应用图标以后再接。 */
export const tint = (name: string) => TINTS[[...name].reduce((h, ch) => (h * 31 + ch.charCodeAt(0)) >>> 0, 7) % TINTS.length];
