import AxeBuilder from '@axe-core/playwright'
import { expect, type Page, test } from '@playwright/test'

/**
 * Axe scans that fail on what they find.
 *
 * A violation of impact "serious" or "critical" fails the test unless its rule
 * is in ALLOWED below with the reason it was accepted. Lower impacts are
 * logged, not failed. An entry in ALLOWED is a reviewed decision, not a way to
 * make a red run green: name the pages it covers and why it cannot be fixed
 * now, and remove it once it is.
 */

type Impact = 'minor' | 'moderate' | 'serious' | 'critical'

const FAILING_IMPACTS: ReadonlySet<Impact> = new Set(['serious', 'critical'])

/** Rule id -> why it is accepted for now. */
const ALLOWED: Readonly<Record<string, string>> = {}

interface Finding {
  id: string
  impact: string | null | undefined
  targets: string[]
}

/** The serious and critical violations not accepted in ALLOWED. */
function blocking(
  violations: Awaited<ReturnType<AxeBuilder['analyze']>>['violations']
): Finding[] {
  return violations
    .filter((v) => FAILING_IMPACTS.has(v.impact as Impact) && !(v.id in ALLOWED))
    .map((v) => ({
      id: v.id,
      impact: v.impact,
      targets: v.nodes.map((n) => n.target.join(' ')),
    }))
}

async function scan(page: Page, configure: (axe: AxeBuilder) => AxeBuilder) {
  const results = await configure(new AxeBuilder({ page })).analyze()
  const logged = results.violations.filter((v) => !FAILING_IMPACTS.has(v.impact as Impact))
  if (logged.length > 0) {
    console.log(
      'Accessibility findings below the failing impact:',
      logged.map((v) => `${v.id} (${v.impact}): ${v.nodes.length} element(s)`)
    )
  }
  return blocking(results.violations)
}

const wcag = (axe: AxeBuilder) => axe.withTags(['wcag2a', 'wcag2aa'])

test.describe('Accessibility', () => {
  test('home page accessibility scan', async ({ page }) => {
    await page.goto('/')
    await page.waitForLoadState('networkidle')

    expect(await scan(page, wcag)).toEqual([])
  })

  test('login page accessibility scan', async ({ page }) => {
    await page.goto('/login')
    await page.waitForLoadState('networkidle')

    expect(await scan(page, wcag)).toEqual([])
  })

  test('color contrast check', async ({ page }) => {
    await page.goto('/')
    await page.waitForLoadState('networkidle')

    expect(await scan(page, (axe) => axe.withRules(['color-contrast']))).toEqual([])
  })

  // The check itself must be able to fail: a nameless button is a critical
  // "button-name" violation, so the same scan that passes the real page has
  // to report it once one is added.
  test('the scan reports a violation it is shown', async ({ page }) => {
    await page.goto('/login')
    await page.waitForLoadState('networkidle')
    await page.evaluate(() => {
      const button = document.createElement('button')
      button.id = 'axe-self-test'
      document.body.appendChild(button)
    })

    const found = await scan(page, wcag)
    expect(found).toContainEqual(
      expect.objectContaining({ id: 'button-name', targets: ['#axe-self-test'] })
    )
  })
})

/**
 * The signed-in pages a trader works in, with the signed-in shell (the
 * session is stubbed; every other call fails, so lists show their empty or
 * error states). Each must have no serious or critical finding beyond the
 * ones in KNOWN for it, and every KNOWN one must still be there: fixing one
 * means removing it here, and a new one fails.
 */
const SIGNED_IN_PAGES = [
  '/dashboard',
  '/orderbook',
  '/tradebook',
  '/positions',
  '/holdings',
  '/strategy',
  '/sandbox',
  '/logs',
  '/profile',
  '/tools',
  '/health',
  '/flow',
  '/flow/shortcuts',
] as const

/**
 * Findings on the signed-in pages still open, rule id per page. Empty: the
 * ones carried over from OpenAlgo web (recorded 2026-10-20) are fixed here,
 * and the same edits are listed for the web in docs/web-sync/a11y-2026-10-20.md.
 * A new finding fails; one accepted for a reason goes here with that reason.
 */
const KNOWN: Readonly<Partial<Record<(typeof SIGNED_IN_PAGES)[number], readonly string[]>>> = {}

async function signedIn(page: Page, theme: 'light' | 'dark' = 'light') {
  if (theme === 'dark') {
    // The theme store's persisted choice, as a trader who picked dark has it.
    await page.emulateMedia({ colorScheme: 'dark' })
    await page.addInitScript(() =>
      localStorage.setItem(
        'openalgo-theme',
        JSON.stringify({ state: { mode: 'dark', color: 'default', appMode: 'live' }, version: 0 })
      )
    )
  }
  await page.route('**/auth/session-status', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        status: 'success',
        logged_in: true,
        authenticated: true,
        broker: 'angel',
        username: 'e2e',
        active_sessions: 1,
      }),
    })
  )
}

/** Health answers with a check in each state, as the stub's failures cannot show. */
async function stubHealth(page: Page) {
  const json = (body: unknown) => ({
    status: 200,
    contentType: 'application/json',
    body: JSON.stringify(body),
  })
  const feed = {
    name: 'market_data',
    connected: true,
    connected_for_s: 600,
    last_update_age_s: 1,
    received_since_connect: 12000,
    skipped_since_connect: {
      strategy: 0,
      scalping: 0,
      sandbox: 0,
      feed_clients: 0,
      order_relay: 0,
    },
    skipped_total_since_connect: 0,
    skipped_since_start: { strategy: 3, scalping: 7, sandbox: 0, feed_clients: 2, order_relay: 0 },
    skipped_total_since_start: 12,
  }
  await page.route('**/health/api/current', (route) =>
    route.fulfill(
      json({
        timestamp: new Date().toISOString(),
        sample_age_s: 5,
        fd: { count: 120, limit: 10240, usage_percent: 1.2, status: 'pass' },
        memory: {
          rss_mb: 210,
          vms_mb: 900,
          percent: 2.5,
          available_mb: 4000,
          swap_mb: 0,
          status: 'warn',
        },
        database: {
          total: 3,
          connections: { openalgo: 2, logs: 1 },
          status: 'pass',
          sizes_mb: { openalgo: 2.5, logs: 40, sandbox: 1, historify: 120 },
        },
        websocket: { total: 1, connections: {}, total_symbols: 12, status: 'pass' },
        threads: { count: 40, stuck: 0, status: 'pass' },
        processes: [],
        overall_status: 'fail',
        order_facts: {
          status: 'fail',
          gaps: [
            { mode: 'live', cause: 'order_feed_reconnected', since: new Date().toISOString() },
          ],
          running: true,
          passes: 3,
          book_reads: 3,
          book_read_failures: 1,
          book_reads_discarded: 0,
          gaps_opened: 1,
          gaps_closed: 0,
          unconfirmed_orders: 1,
        },
        records: {
          status: 'warn',
          order_logs_dropped: 2,
          order_logs_failed: 0,
          monitoring_dropped: 0,
          monitoring_lost: 0,
          monitoring_retried_batches: 1,
        },
        strategy_ticks: {
          status: 'warn',
          ticks_queued: 50,
          ticks_folded: 4,
          ticks_missed: 2,
          exits_waiting: 1,
          oldest_exit_wait_s: 3,
          exits_slow: false,
        },
        market_data: {
          status: 'warn',
          broker_feeds: [feed],
          feed_clients: { coalesced: 30, dropped: 1, skipped: 0 },
        },
      })
    )
  )
  const alert = (id: number, alert_type: string, severity: string, resolvable: boolean) => ({
    id,
    timestamp: new Date().toISOString(),
    alert_type,
    severity,
    metric_name: alert_type,
    metric_value: 1,
    threshold_value: 0,
    message: `${alert_type.replace(/_/g, ' ')}: what happened and what to do`,
    acknowledged: false,
    resolved: false,
    resolvable,
  })
  await page.route('**/health/api/alerts', (route) =>
    route.fulfill(
      json([
        alert(7, 'records_not_saved', 'warn', true),
        alert(8, 'order_facts_gap', 'fail', false),
      ])
    )
  )
  await page.route('**/health/api/held-orders', (route) =>
    route.fulfill(
      json({
        status: 'success',
        data: [
          {
            owner: 'strategy',
            id: '42',
            symbol: 'NIFTY07OCT2624500CE',
            exchange: 'NFO',
            action: 'BUY',
            quantity: 65,
            since: '2026-10-07 10:00:20',
            message: 'It may still be at the broker.',
          },
        ],
      })
    )
  )
  await page.route('**/health/api/history**', (route) => route.fulfill(json([])))
  await page.route('**/health/api/stats**', (route) => route.fulfill({ status: 500, body: '' }))
}

/** One stored workflow for the Flow editor, as GET /flow/api/workflows/7 answers. */
async function stubFlowWorkflow(page: Page) {
  const workflow = {
    id: 7,
    name: 'Opening range breakout',
    description: null,
    nodes: [
      {
        id: 'node_1',
        type: 'start',
        position: { x: 0, y: 0 },
        data: { scheduleType: 'daily', time: '09:20' },
      },
      {
        id: 'node_2',
        type: 'timeWindow',
        position: { x: 0, y: 160 },
        data: { startTime: '09:20', endTime: '15:00' },
      },
      {
        id: 'node_3',
        type: 'placeOrder',
        position: { x: -160, y: 340 },
        data: { symbol: 'SBIN', exchange: 'NSE', action: 'BUY', quantity: 1, priceType: 'MARKET' },
      },
      {
        id: 'node_4',
        type: 'log',
        position: { x: 160, y: 340 },
        data: { message: 'Outside the window', level: 'info' },
      },
    ],
    edges: [
      { id: 'e1', source: 'node_1', target: 'node_2' },
      { id: 'e2', source: 'node_2', sourceHandle: 'true', target: 'node_3' },
      { id: 'e3', source: 'node_2', sourceHandle: 'false', target: 'node_4' },
    ],
    is_active: false,
    schedule_job_id: null,
    webhook_token: null,
    webhook_secret: null,
    webhook_enabled: false,
    webhook_auth_type: 'payload',
    created_at: '2026-10-30T09:00:00',
    updated_at: '2026-10-30T09:00:00',
  }
  await page.route('**/flow/api/workflows/7', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(workflow) })
  )
  await page.route('**/flow/api/index-symbols', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ status: 'success', data: [] }),
    })
  )
}

test.describe('Accessibility - signed in', () => {
  for (const theme of ['light', 'dark'] as const) {
    for (const path of SIGNED_IN_PAGES) {
      test(`${path} accessibility scan${theme === 'dark' ? ' (dark)' : ''}`, async ({ page }) => {
        await signedIn(page, theme)
        await page.goto(path)
        await page.waitForLoadState('networkidle')
        await expect(page.getByTestId('navbar-row')).toBeVisible()

        const found = [...new Set((await scan(page, wcag)).map((f) => f.id))].sort()
        expect(found).toEqual([...(KNOWN[path] ?? [])].sort())
      })
    }
  }

  // The Flow editor has no navbar (full width, its own header) and needs a
  // workflow to draw, so it is scanned with one: a schedule, a condition
  // with both branches and an order, which shows every handle colour.
  for (const theme of ['light', 'dark'] as const) {
    test(`/flow/editor with a workflow${theme === 'dark' ? ' (dark)' : ''}`, async ({ page }) => {
      await signedIn(page, theme)
      await stubFlowWorkflow(page)
      await page.goto('/flow/editor/7')
      await page.waitForLoadState('networkidle')
      await expect(page.getByRole('textbox', { name: 'Workflow name' })).toBeVisible()
      await expect(page.locator('.react-flow__node')).toHaveCount(4)

      expect(await scan(page, wcag)).toEqual([])
    })
  }

  // States the stub does not reach by itself: each master contract status
  // the dashboard shows, and the password rules once some are met.
  for (const status of ['success', 'downloading', 'pending']) {
    test(`/dashboard with the master contract ${status}`, async ({ page }) => {
      await signedIn(page)
      await page.route('**/api/master-contract/status', (route) =>
        route.fulfill({
          status: 200,
          contentType: 'application/json',
          body: JSON.stringify({ status, message: `Master contract ${status}` }),
        })
      )
      await page.goto('/dashboard')
      await page.waitForLoadState('networkidle')

      expect(await scan(page, wcag)).toEqual([])
    })
  }

  // The Health page with what it shows when something is wrong (PROD-PAR-01):
  // amber and red checks with their reasons, alerts with Acknowledge and
  // Resolve, an unconfirmed order, and a list it could not read. The stub
  // above covers every read failing.
  for (const theme of ['light', 'dark'] as const) {
    test(`/health with failing checks, alerts and an unconfirmed order${theme === 'dark' ? ' (dark)' : ''}`, async ({
      page,
    }) => {
      await signedIn(page, theme)
      await stubHealth(page)
      await page.goto('/health')
      await page.waitForLoadState('networkidle')
      await expect(page.getByText('Unconfirmed Orders (1)')).toBeVisible()
      await expect(page.getByRole('button', { name: 'Resolve' })).toBeVisible()
      await expect(page.getByText(/Could not read the statistics/)).toBeVisible()

      expect(await scan(page, wcag)).toEqual([])
    })
  }

  test('/profile with a new password typed', async ({ page }) => {
    await signedIn(page)
    await page.goto('/profile')
    await page.waitForLoadState('networkidle')
    await page.getByLabel('New Password', { exact: true }).fill('abcDEF12')

    expect(await scan(page, wcag)).toEqual([])
  })
})

test.describe('Accessibility - Mobile', () => {
  test.use({ viewport: { width: 375, height: 667 } })

  test('mobile layout accessibility scan', async ({ page }) => {
    await page.goto('/')
    await page.waitForLoadState('networkidle')

    expect(await scan(page, wcag)).toEqual([])
  })
})
