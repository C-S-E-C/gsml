import { defineConfig } from 'vite';

export default defineConfig({
  root: '.',
  clearScreen: false,
  server: {
    port: 5533,
    strictPort: true,
    watch: { ignored: ['**/src-tauri/**'] },
  },
  build: {
    target: 'esnext',
    rollupOptions: {
      input: {
        main: 'index.html',
        home: 'home.html',
        download: 'download.html',
        plugins: 'plugins.html',
        connect: 'connect.html',
        settings: 'settings.html',
        'user-license': 'user_license.html',
      },
    },
  },
  optimizeDeps: { include: ['@material/web/all.js'] },
});