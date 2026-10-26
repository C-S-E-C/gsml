import { defineConfig } from 'vite';
import { resolve } from 'node:path';

// Multi-page build: every top-level .html under the project root is a page
// entry. index.html is the launcher shell; the rest are the iframed pages
// (home, download, plugins, connect, settings) plus the user_license gate.
// Each entry carries its own css/js, resolved from ./src by Vite, so the
// built output is fully self-contained under ../ (no ../src/js.. refs).
export default defineConfig({
  root: '.',
  clearScreen: false,
  server: {
    port: 5533,
    strictPort: true,          // 端口被占时直接报错，不偷偷换端口
    watch: { ignored: ['**/src-tauri/**'] },
  },
  build: {
    target: 'esnext',
    rollupOptions: {
      input: {
        main: resolve('index.html'),
        home: resolve('home.html'),
        download: resolve('download.html'),
        plugins: resolve('plugins.html'),
        connect: resolve('connect.html'),
        settings: resolve('settings.html'),
        'user-license': resolve('user_license.html'),
      },
    },
  },
  optimizeDeps: { include: ['@material/web/all.js'] },
});
