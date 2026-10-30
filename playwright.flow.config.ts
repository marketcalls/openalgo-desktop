import { defineConfig, devices } from '@playwright/test'

/**
 * Flow's end-to-end journeys against the app's real Rust server (CI job
 * `e2e-flow`). Unlike playwright.config.ts (a browser against `vite preview`
 * with no backend), `e2e/flow/support/global-setup.ts` starts the
 * `e2e_server` example on a port the system picks, over a fresh data folder,
 * with the scripted broker of CI's native test build; it creates the account,
 * signs in, logs in to the scripted broker and turns analyzer mode on, and
 * stops the server when the run ends.
 *
 * Build first: `npx vite build`, then from src-tauri
 * `cargo build --example e2e_server --features e2e-broker`. The binary is
 * found at OPENALGO_E2E_SERVER_BIN, else in src-tauri/target/debug/examples.
 *
 * One worker, in file order: the journeys share one server and one account,
 * and the Agent's model is the account's.
 */
export default defineConfig({
  testDir: './e2e/flow',
  testMatch: '*.spec.ts',
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  // A release gate: a journey that needs a retry is a bug to fix, not to hide.
  retries: 0,
  timeout: 120_000,
  expect: { timeout: 20_000 },
  reporter: [['list'], ['html', { open: 'never', outputFolder: 'playwright-report-flow' }]],
  outputDir: 'test-results/flow',
  globalSetup: './e2e/flow/support/global-setup.ts',
  use: {
    ...devices['Desktop Chrome'],
    viewport: { width: 1440, height: 900 },
    // baseURL and the signed-in storage state come from the global setup,
    // through the fixtures in e2e/flow/support/fixtures.ts.
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
    acceptDownloads: true,
  },
  projects: [{ name: 'chromium' }],
})
