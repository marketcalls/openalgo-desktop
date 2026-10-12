import path from 'path'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

export default defineConfig({
  plugins: [react()],
  oxc: {
    jsx: {
      runtime: 'automatic',
      importSource: 'react',
    },
  },
  test: {
    globals: true,
    environment: 'jsdom',
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/**/*.{test,spec}.{js,mjs,cjs,ts,mts,cts,jsx,tsx}'],
    exclude: ['node_modules', 'dist', '.idea', '.git', '.cache'],
    coverage: {
      provider: 'v8',
      reporter: ['text', 'json', 'json-summary', 'html'],
      // Desktop: every source file counts, also the ones no test imports.
      // Without this the denominator is only the files the tests happen to
      // load, which reads about 18 points higher than the truth.
      include: ['src/**/*.{ts,tsx}'],
      // Desktop: only what never runs in the app is left out: test helpers,
      // declaration files and config. Files under types/ stay in: some hold
      // executable helpers (strategy_module.ts, option-chain.ts), and the
      // rest are type-only, which adds nothing to either side of the ratio.
      exclude: ['node_modules/', 'src/test/', '**/*.d.ts', '**/*.config.*'],
      // Desktop: a ratchet, so CI fails when coverage drops. Set from what
      // CI itself measured on 2026-10-12 (PR #42, run 38272962752), rounded
      // down: the totals and directories 1 point under it, the trading
      // areas 2 points under the lowest of CI's Node 22 run and a Node 24
      // run. V8's counts move by up to about a point between Node versions
      // and runs on these small groups (functions on the book pages: 48.55
      // on Node 22, 47.82 on CI's Node 24), and a floor at the measured
      // value fails at random. Raise these as coverage rises; never lower
      // one, and never exclude a file to meet it. The trading areas have
      // floors of their own, so a drop there cannot hide behind a rise
      // elsewhere.
      thresholds: {
        lines: 54,
        statements: 53,
        functions: 48,
        branches: 48,
        'src/lib/**': { lines: 82, statements: 78, functions: 82, branches: 72 },
        'src/stores/**': { lines: 31, statements: 31, functions: 39, branches: 21 },
        'src/hooks/**': { lines: 72, statements: 70, functions: 70, branches: 58 },
        'src/types/**': { lines: 92, statements: 90, functions: 92, branches: 83 },
        // Orders: the book pages, order entry and the terminal's order dock.
        'src/pages/{OrderBook,TradeBook,Positions}.tsx': {
          lines: 51,
          statements: 49,
          functions: 46,
          branches: 41,
        },
        'src/components/trading/{PlaceOrderDialog,ExecuteBasketDialog,GttTab,dock/*}.{ts,tsx}': {
          lines: 55,
          statements: 53,
          functions: 45,
          branches: 49,
        },
        // Strategies: the strategy module and the strategy builder.
        'src/{pages/strategy/*,api/strategy_module,types/strategy_module,lib/trading/strategy*}.{ts,tsx}':
          { lines: 55, statements: 54, functions: 45, branches: 43 },
        'src/{pages/StrategyBuilder,components/strategy-builder/*,lib/strategyMath,lib/strategyTemplates}.{ts,tsx}':
          { lines: 87, statements: 84, functions: 81, branches: 77 },
        // Scalping and its protection (stop-loss, target, trailing).
        'src/{pages/Scalping,components/scalping/*,hooks/useTrailingSL,lib/scalping*,api/scalping}.{ts,tsx}':
          { lines: 7, statements: 7, functions: 1, branches: 9 },
        // Broker connection pages.
        'src/{pages/BrokerSelect,pages/BrokerTOTP,components/auth/Broker*,stores/brokerStore}.{ts,tsx}':
          { lines: 58, statements: 58, functions: 69, branches: 56 },
      },
    },
    css: true,
  },
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
})
