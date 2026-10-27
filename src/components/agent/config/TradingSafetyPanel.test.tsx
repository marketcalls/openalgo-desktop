import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { AgentSettings } from '@/api/agent'
import { changesOf, draftProblem, SYSTEM_PROMPT_TOO_LONG } from './SafetyLimitsForm'
import { TradingSafetyPanel } from './TradingSafetyPanel'

const settings: AgentSettings = {
  trading_enabled: false,
  require_analyzer_mode: false,
  system_prompt: null,
  default_reasoning_effort: 'off',
  max_orders_per_session: 20,
  max_order_quantity: 10000,
  max_order_value: '500000.00',
  max_price_deviation_pct: '5',
  duplicate_order_window_seconds: 10,
  allowed_exchanges: ['BSE', 'NSE'],
  allowed_products: ['CNC', 'MIS', 'NRML'],
  symbol_allowlist: [],
  symbol_blocklist: [],
  max_funds_utilization_pct: '100',
  allow_bulk_destructive: false,
  kill_switch: false,
  kill_switch_file: '/data/agent_kill_switch',
  mcp_enabled: true,
}

const api = vi.hoisted(() => ({
  getSettings: vi.fn(),
  updateSettings: vi.fn(),
  getKillSwitch: vi.fn(),
  setKillSwitch: vi.fn(),
}))

vi.mock('@/api/agent', async (original) => ({
  ...(await original<typeof import('@/api/agent')>()),
  getSettings: api.getSettings,
  updateSettings: api.updateSettings,
}))
vi.mock('@/api/agentSafety', async (original) => ({
  ...(await original<typeof import('@/api/agentSafety')>()),
  getKillSwitch: api.getKillSwitch,
  setKillSwitch: api.setKillSwitch,
}))

function draft(over: Partial<Record<string, unknown>> = {}) {
  return {
    max_orders_per_session: '20',
    max_order_quantity: '10000',
    max_order_value: '500000.00',
    max_price_deviation_pct: '5',
    duplicate_order_window_seconds: '10',
    max_funds_utilization_pct: '100',
    allowed_exchanges: 'BSE, NSE',
    allowed_products: 'CNC, MIS, NRML',
    symbol_allowlist: '',
    symbol_blocklist: '',
    kill_switch_file: '/data/agent_kill_switch',
    system_prompt: '',
    require_analyzer_mode: false,
    allow_bulk_destructive: false,
    default_reasoning_effort: 'off' as const,
    ...over,
  } as Parameters<typeof changesOf>[0]
}

describe('the trading safety limits', () => {
  it('sends only what changed, lists compared as the server stores them', () => {
    expect(changesOf(draft(), settings)).toEqual({})
    expect(changesOf(draft({ allowed_exchanges: 'nse,bse' }), settings)).toEqual({})
    expect(
      changesOf(draft({ max_order_quantity: '500', allow_bulk_destructive: true }), settings)
    ).toEqual({ max_order_quantity: '500', allow_bulk_destructive: true })
  })

  it('refuses what the server would refuse, in trader words', () => {
    expect(draftProblem(draft())).toBeNull()
    expect(draftProblem(draft({ max_order_quantity: 'ten' }))).toMatch(/whole number/)
    expect(draftProblem(draft({ max_price_deviation_pct: '101' }))).toMatch(/at most 100/)
    expect(draftProblem(draft({ system_prompt: 'a'.repeat(20_001) }))).toBe(SYSTEM_PROMPT_TOO_LONG)
  })
})

describe('TradingSafetyPanel', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    api.getSettings.mockResolvedValue({ data: settings, defaults: settings })
    api.getKillSwitch.mockResolvedValue({
      engaged: false,
      by_setting: false,
      by_file: false,
      path: '/data/agent_kill_switch',
    })
  })

  function mount() {
    const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
    return render(
      <QueryClientProvider client={client}>
        <TradingSafetyPanel />
      </QueryClientProvider>
    )
  }

  it('engages the kill switch from the panel and says where its file is', async () => {
    api.setKillSwitch.mockResolvedValue({
      engaged: true,
      by_setting: true,
      by_file: true,
      path: '/data/agent_kill_switch',
    })
    mount()
    const toggle = await screen.findByRole('switch', { name: /kill switch/i })
    expect(screen.getByText('/data/agent_kill_switch')).toBeInTheDocument()
    fireEvent.click(toggle)
    await waitFor(() => expect(api.setKillSwitch).toHaveBeenCalledWith(true))
    expect(await screen.findByText(/Engaged\./)).toBeInTheDocument()
  })

  it('says the file keeps the switch engaged when only the file is there', async () => {
    api.getKillSwitch.mockResolvedValue({
      engaged: true,
      by_setting: false,
      by_file: true,
      path: '/data/agent_kill_switch',
    })
    mount()
    expect(await screen.findByText(/The file is there/)).toBeInTheDocument()
  })

  it('carries a Semi-Auto sentence and no forbidden words', async () => {
    const { container } = mount()
    await screen.findByRole('switch', { name: /kill switch/i })
    expect(container.textContent).toMatch(/Semi-Auto/)
    expect(container.textContent).toMatch(/Action Center/)
    expect(container.textContent ?? '').not.toMatch(/\barm(ed|ing)?\b|paper|virtual|LiteLLM/i)
  })
})
