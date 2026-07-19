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
  '@vibecook/mille',
  '@vibecook/mille/host',
];

export default defineConfig({
  main: {
    plugins: [externalizeDepsPlugin()],
    build: {
      // Multi-entry: main window process + isolated utility hosts.
      rollupOptions: {
        input: {
          index: resolve(__dirname, 'src/main/index.ts'),
          'fx-host': resolve(__dirname, 'src/utility/fx-host.ts'),
          'sdk-host': resolve(__dirname, 'src/utility/sdk-host.ts'),
        },
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
    // Don't pull Node natives into the renderer bundle. Also keep
    // file:-linked @vibecook/mille-ui out of the prebundle cache so local
    // mille rebuilds (e.g. single-click folder expand) are picked up
    // without a stale .vite/deps snapshot of double-click expand.
    optimizeDeps: {
      exclude: ['@vibecook/mille', '@vibecook/mille-ui', 'better-sqlite3'],
    },
  },
});
