import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import dts from 'vite-plugin-dts';
import { resolve } from 'path';

export default defineConfig({
  plugins: [react(), dts({ entryRoot: 'src', insertTypesEntry: true, rollupTypes: true })],
  build: {
    lib: {
      entry: {
        index: resolve(__dirname, 'src/index.ts'),
        client: resolve(__dirname, 'src/client/portable.ts'),
        observation: resolve(__dirname, 'src/observation.ts'),
        react: resolve(__dirname, 'src/react/index.ts'),
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
