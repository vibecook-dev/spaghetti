import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import dts from 'vite-plugin-dts';
import { resolve } from 'path';

export default defineConfig({
  plugins: [
    react(),
    dts({ entryRoot: 'src', insertTypesEntry: true, rollupTypes: false }),
  ],
  build: {
    lib: {
      entry: {
        index: resolve(__dirname, 'src/index.ts'),
        react: resolve(__dirname, 'src/react/index.ts'),
        // Emitted so `dist/parse-worker.js` sits next to `dist/index.js`,
        // which is where WorkerPool looks for it. Without this entry the
        // published package shipped no worker script at all, so parallel
        // cold start could never run — see the note in worker-pool.ts.
        // `.ts` is explicit: a generated `parse-worker.js` sits beside the
        // source for the from-src dev path, and would otherwise win
        // resolution here.
        'parse-worker': resolve(__dirname, 'src/workers/parse-worker.ts'),
      },
      formats: ['es', 'cjs'],
      fileName: (format, entryName) => `${entryName}.${format === 'es' ? 'js' : 'cjs'}`,
    },
    rollupOptions: {
      external: [
        'better-sqlite3',
        '@parcel/watcher',
        /^@parcel\/watcher-/,
        'chokidar',
        'ws',
        'js-tiktoken',
        'react',
        'react-dom',
        'react/jsx-runtime',
        'react-markdown',
        'remark-gfm',
        'react-syntax-highlighter',
        /^react-syntax-highlighter\//,
        'lucide-react',
        'clsx',
        'tailwind-merge',
        'class-variance-authority',
        /^node:/,
        'events',
        'fs',
        'fs/promises',
        'path',
        'os',
        'crypto',
        'worker_threads',
      ],
    },
  },
});
