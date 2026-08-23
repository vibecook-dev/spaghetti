import eslint from '@eslint/js';
import tseslint from 'typescript-eslint';

export default tseslint.config(
  eslint.configs.recommended,
  ...tseslint.configs.recommended,
  {
    rules: {
      '@typescript-eslint/no-explicit-any': 'off',
      '@typescript-eslint/no-unused-vars': ['error', { argsIgnorePattern: '^_' }],
    },
  },
  {
    // packages/sdk/src/generated is ts-rs output; see scripts/generate-types.mjs.
    ignores: ['**/dist/**', '**/node_modules/**', '**/*.js', '**/*.cjs', 'packages/sdk/src/generated/**'],
  },
);
