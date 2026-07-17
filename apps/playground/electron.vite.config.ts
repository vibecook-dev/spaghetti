import { defineConfig, externalizeDepsPlugin } from 'electron-vite';
import react from '@vitejs/plugin-react';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));

// Native / Node-only packages must stay external so esbuild never tries to
// load `.node` binaries (and so the renderer cannot pull them via HMR).
const mainExternals = [
  'better-sqlite3',
  '@parcel/watcher',
  '@vibecook/spaghetti-sdk',
  '@vibecook/spaghetti-sdk-native',
];

export default defineConfig({
  main: {
    plugins: [externalizeDepsPlugin()],
    build: {
      lib: { entry: resolve(__dirname, 'src/main/index.ts') },
      rollupOptions: {
        external: mainExternals,
      },
    },
    resolve: {
      alias: { '@shared': resolve(__dirname, 'src/shared') },
    },
  },
  preload: {
    plugins: [externalizeDepsPlugin()],
    build: {
      lib: { entry: resolve(__dirname, 'src/preload/index.ts') },
    },
    resolve: {
      alias: { '@shared': resolve(__dirname, 'src/shared') },
    },
  },
  renderer: {
    plugins: [react()],
    root: resolve(__dirname, 'src/renderer'),
    build: {
      rollupOptions: {
        input: resolve(__dirname, 'src/renderer/index.html'),
      },
    },
    resolve: {
      alias: {
        '@shared': resolve(__dirname, 'src/shared'),
        '@': resolve(__dirname, 'src/renderer/src'),
      },
    },
  },
});
