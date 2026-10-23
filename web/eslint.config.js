import js from '@eslint/js'
import globals from 'globals'
import reactHooks from 'eslint-plugin-react-hooks'
import tseslint from 'typescript-eslint'

export default tseslint.config(
  {
    // Generated code is the contract's output, not source to lint.
    ignores: ['dist', 'src/generated', 'node_modules', 'playwright-report', 'test-results'],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      ecmaVersion: 2022,
      globals: { ...globals.browser, ...globals.node },
    },
    plugins: { 'react-hooks': reactHooks },
    rules: {
      ...reactHooks.configs.recommended.rules,
      // Unused variables are usually a mistake; an underscore prefix is the
      // explicit way to say "deliberately ignored".
      '@typescript-eslint/no-unused-vars': [
        'error',
        { argsIgnorePattern: '^_', varsIgnorePattern: '^_' },
      ],
      // `any` erases exactly the safety the generated contract provides.
      '@typescript-eslint/no-explicit-any': 'error',
      // Production code must not log message content to the console (§85).
      'no-console': ['warn', { allow: ['warn', 'error'] }],
    },
  },
  {
    // Tests may reach into internals to drive a module deliberately.
    files: ['**/*.test.{ts,tsx}', 'src/test/**'],
    rules: { '@typescript-eslint/no-explicit-any': 'off' },
  },
  {
    // Build tooling runs in Node, not the browser, and reporting a lint
    // failure is exactly what its console output is for.
    files: ['scripts/**/*.mjs'],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: 'module',
      globals: { ...globals.node },
    },
    rules: { 'no-console': 'off' },
  },
)
