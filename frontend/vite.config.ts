import react from '@vitejs/plugin-react';
import { resolve } from 'node:path';
import { defineConfig } from 'vite';

const root = import.meta.dirname;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  // 端口与绑定地址见 docs/rules/project.md：仅本机访问，默认 18540。
  // 18540 被别的项目占用时用 `make dev PORT=18541` 换一个，Makefile 会同步告诉 Tauri。
  server: { host: '127.0.0.1', port: Number(process.env.PIKA_DEV_PORT ?? 18540), strictPort: true },
  build: {
    target: 'safari16',
    rollupOptions: { input: { main: resolve(root, 'index.html'), popover: resolve(root, 'popover.html') } },
  },
});
