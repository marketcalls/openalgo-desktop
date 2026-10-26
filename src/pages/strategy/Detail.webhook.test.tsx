import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

// docs-37: the desktop's pages are served from 127.0.0.1, so the strategy
// webhook address shows the Host Server URL (or the built-in tunnel's while
// it is online), never the page's own address. docs-5: the IP allowlist text
// says what the server does with a tunnel caller.

const rest = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }))

vi.mock('@/api/client', () => ({
  webClient: { get: rest.get, post: rest.post, patch: vi.fn(), delete: vi.fn() },
  apiClient: { post: vi.fn(), get: vi.fn() },
  authClient: { post: vi.fn() },
  fetchCSRFToken: vi.fn(),
  default: { post: vi.fn(), get: vi.fn() },
}))

vi.mock('@/api/strategy_module', async () => {
  const actual =
    await vi.importActual<typeof import('@/api/strategy_module')>('@/api/strategy_module')
  return {
    ...actual,
    useStrategyLive: () => ({
      status: 'polling',
      runId: null,
      checkpoint: null,
      legs: [],
      updatedAt: null,
      curve: [],
      isFetching: false,
      error: null,
      refresh: vi.fn(),
    }),
  }
})

vi.mock('@/utils/toast', () => ({
  showToast: { success: vi.fn(), error: vi.fn(), warning: vi.fn(), info: vi.fn() },
}))

import type { Strategy } from '@/types/strategy_module'
import StrategyDetail from './Detail'

const strategy = {
  id: 7,
  name: 'Webhook address',
  strategy_kind: 'batch',
  direction: 'both',
  universe_tab: 'weekly_monthly',
  underlying: 'NIFTY',
  underlying_exchange: 'NSE_INDEX',
  strategy_type: 'intraday',
  entry_time: null,
  exit_time: null,
  product: 'NRML',
  pricetype: 'MARKET',
  overall_sl_mtm: null,
  overall_target_mtm: null,
  lock_profit: null,
  trail_sl_to_entry: false,
  scheduler: null,
  live_enabled: false,
  webhook_locked: false,
  webhook_ip_allowlist: null,
  daily_loss_limit_inr: null,
  status: 'stopped',
  current_run_id: null,
  created_at: '2026-05-28T03:45:00+00:00',
  updated_at: '2026-05-28T03:45:00+00:00',
  legs: [],
} satisfies Strategy

beforeEach(() => {
  rest.get.mockImplementation((url: string) =>
    Promise.resolve(
      url === '/strategy/api/strategies/7' ? { data: { data: strategy } } : { data: { data: [] } }
    )
  )
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) =>
      url === '/api/config/host'
        ? new Response(JSON.stringify({ host_server: 'https://desk.ngrok-free.app' }))
        : new Response('{}', { status: 404 })
    )
  )
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.clearAllMocks()
})

describe('strategy webhook address', () => {
  it('uses the Host Server URL and states the allowlist rule', async () => {
    const user = userEvent.setup()
    render(
      <QueryClientProvider
        client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}
      >
        <MemoryRouter initialEntries={['/strategy/7']}>
          <Routes>
            <Route path="/strategy/:strategyId" element={<StrategyDetail />} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>
    )
    await screen.findByText('Webhook address')
    await user.click(screen.getByRole('tab', { name: 'Webhook' }))
    const url = await screen.findByDisplayValue(/\/strategy\/webhook\//)
    await vi.waitFor(() =>
      expect((url as HTMLInputElement).value).toBe(
        'https://desk.ngrok-free.app/strategy/webhook/••••••••••••'
      )
    )
    expect(
      screen.getByText(/while the list has entries those calls are refused/)
    ).toBeInTheDocument()
    expect(screen.queryByText(/an IP allowlist does not apply there/)).toBeNull()
  })
})
