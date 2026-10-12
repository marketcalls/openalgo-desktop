/**
 * The trade book: the day's fills, their counts, and the filters that narrow
 * both the table and the counts.
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'
import { render, screen, userEvent, within } from '@/test/test-utils'
import type { Trade } from '@/types/trading'

const mocks = vi.hoisted(() => ({ getTrades: vi.fn() }))

vi.mock('@/api/trading', () => ({ tradingApi: mocks }))
vi.mock('@/stores/authStore', () => ({
  useAuthStore: () => ({ apiKey: 'test-api-key', user: { broker: 'zerodha' } }),
}))
vi.mock('@/stores/themeStore', () => ({ onModeChange: () => () => {} }))
vi.mock('@/hooks/useOrderEventRefresh', () => ({ useOrderEventRefresh: () => {} }))
vi.mock('@/hooks/useSupportedExchanges', () => ({
  useSupportedExchanges: () => ({ isCrypto: false }),
}))

import TradeBook from './TradeBook'

const trade = (t: Partial<Trade>): Trade => ({
  symbol: 'SBIN',
  exchange: 'NSE',
  action: 'BUY',
  quantity: 10,
  average_price: 800,
  trade_value: 8000,
  product: 'MIS',
  orderid: 'T1',
  timestamp: '10-Oct-2026 10:00:00',
  ...t,
})

const TRADES = [
  trade({}),
  trade({ symbol: 'INFY', orderid: 'T2', timestamp: '10-Oct-2026 10:05:00' }),
  trade({
    symbol: 'RELIANCE',
    action: 'SELL',
    orderid: 'T3',
    exchange: 'BSE',
    timestamp: '10-Oct-2026 10:10:00',
  }),
]

function count(label: string): string {
  const card = screen.getByText(label).closest('[data-slot="card"]') as HTMLElement
  return within(card).getByText(/^\d+$/).textContent ?? ''
}

describe('Trade book', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('lists the fills and counts buys and sells', async () => {
    mocks.getTrades.mockResolvedValue({ status: 'success', data: TRADES })
    render(<TradeBook />)

    expect(await screen.findByText('RELIANCE')).toBeInTheDocument()
    expect(screen.getByText('SBIN')).toBeInTheDocument()
    expect(screen.getByText('INFY')).toBeInTheDocument()
    expect(count('Total Trades')).toBe('3')
    expect(count('Buy Trades')).toBe('2')
    expect(count('Sell Trades')).toBe('1')
  })

  it('narrows the table and the counts to the chosen side', async () => {
    mocks.getTrades.mockResolvedValue({ status: 'success', data: TRADES })
    render(<TradeBook />)
    await screen.findByText('RELIANCE')
    const user = userEvent.setup()

    await user.click(screen.getByRole('button', { name: 'Open trade filters' }))
    await user.click(await screen.findByRole('button', { name: 'Sell' }))

    expect(screen.queryByText('SBIN')).not.toBeInTheDocument()
    expect(screen.getByText('RELIANCE')).toBeInTheDocument()
    expect(count('Total Trades')).toBe('1')
    expect(count('Buy Trades')).toBe('0')
  })

  it('says so when the trade book cannot be loaded', async () => {
    mocks.getTrades.mockResolvedValue({ status: 'error', message: 'Failed to fetch trades' })
    render(<TradeBook />)

    expect(await screen.findByText('Failed to fetch trades')).toBeInTheDocument()
  })
})
