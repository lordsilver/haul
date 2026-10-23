import { fileURLToPath } from 'node:url';
import tailwindcss from '@tailwindcss/vite';
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// `pnpm dev` serves the page with hot reload and hands the API to a Haul
// already running (HAUL_URL, by default the port `haul` and the desktop app
// use). Haul refuses browser requests from another origin, so the proxy
// presents them as its own.
const haul = process.env.HAUL_URL ?? 'http://127.0.0.1:8765';

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  clearScreen: false,
  server: {
    host: '127.0.0.1',
    port: 5173,
    proxy: {
      '/api': {
        target: haul,
        changeOrigin: true,
        configure: (proxy) => proxy.on('proxyReq', (req) => req.setHeader('origin', haul)),
      },
    },
  },
  build: {
    target: 'es2024',
    // haul-web embeds every file of dist/ (build.rs), and git keeps dist/ so
    // a Rust-only build needs no Node. Fixed names keep its diffs readable.
    outDir: 'dist',
    emptyOutDir: true,
    assetsInlineLimit: 0,
    rolldownOptions: {
      output: {
        entryFileNames: 'assets/app.js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: 'assets/[name][extname]',
      },
    },
  },
});
