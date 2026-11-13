import { defineConfig } from 'vite';

export default defineConfig({
  base: './',
  optimizeDeps: { exclude: ['@project516/yt-dlp-wasm', '@ffmpeg/ffmpeg'] },
  worker: { format: 'es' },
  assetsInclude: ['**/*.whl'],
  build: { rollupOptions: { output: { assetFileNames: (asset) => asset.names.some((name) => name.endsWith('.whl')) ? 'assets/[name][extname]' : 'assets/[name]-[hash][extname]' } } },
});
