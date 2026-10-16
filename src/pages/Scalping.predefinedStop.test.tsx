/**
 * UI-03 and N-05 on the scalping terminal.
 *
 * The predefined stop-loss promised for every entry is measured from a fresh
 * price: a live tick younger than the page's freshness window, otherwise a
 * quote read when the entry is accepted. With no price at all the trader is
 * told the position has no stop, and an enabled value that cannot be used
 * keeps the entry from being sent. The web read whatever the WebSocket cache
 * held, measured from an old tick, and skipped the stop without a word.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { render, screen, userEvent, waitFor, within } from '@/test/test-utils'
import Scalping from './Scalping'

const CE = 'NIFTY28OCT2625000CE'
const PE = 'NIFTY28OCT2625000PE'

const scalping = vi.hoisted(() => ({
  getUnderlyings: vi.fn(),
  getAllUnderlyings: vi.fn(),
  getExpiry: vi.fn(),
  search: vi.fn(),
  futures: vi.fn(),
  getStrikes: vi.fn(),
  getHistory: vi.fn(),
  placeOrder: vi.fn(),
  closeLeg: vi.fn(),
  closeAll: vi.fn(),
  cancelAll: vi.fn(),
  getTracked: vi.fn(),
  resetTracked: vi.fn(),
  getSLStates: vi.fn(),
  saveSL: vi.fn(),
  deleteSL: vi.fn(),
}))
const trading = vi.hoisted(() => ({
  getPositions: vi.fn(),
  getOrders: vi.fn(),
  getTrades: vi.fn(),
  getMultiQuotes: vi.fn(),
  getQuotes: vi.fn(),
}))
const feed = vi.hoisted(() => ({ data: new Map<string, unknown>() }))
const toast = vi.hoisted(() => ({
  error: vi.fn(),
  success: vi.fn(),
  info: vi.fn(),
  warning: vi.fn(),
}))

vi.mock('@/api/scalping', () => ({ scalpingApi: scalping }))
vi.mock('@/api/trading', () => ({ tradingApi: trading }))
vi.mock('@/hooks/useMarketData', () => ({
  useMarketData: () => ({
    data: feed.data,
    isConnected: true,
    isAuthenticated: true,
    isFallbackMode: false,
  }),
}))
vi.mock('@/hooks/useOrderEventRefresh', () => ({ useOrderEventRefresh: () => {} }))
vi.mock('@/components/scalping/ScalpChart', () => ({ ScalpChart: () => null }))
vi.mock('@/utils/toast', () => ({ showToast: toast }))
vi.mock('@/stores/authStore', () => ({
  useAuthStore: (select: (s: { apiKey: string }) => unknown) => select({ apiKey: 'test-key' }),
}))
vi.mock('@/stores/themeStore', () => ({
  useThemeStore: (select: (s: { appMode: string }) => unknown) => select({ appMode: 'live' }),
}))

function leg(symbol: string) {
  return { symbol, lotsize: 75, tick_size: 0.05, exists: true }
}

function mountPage() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(
    <QueryClientProvider client={client}>
      <Scalping />
    </QueryClientProvider>
  )
}

async function predefinedStop(value: string) {
  const box = screen.getByTitle('Auto stop-loss on every entry')
  await userEvent.click(within(box).getByRole('checkbox'))
  if (value) await userEvent.type(within(box).getByRole('spinbutton'), value)
}

async function buyCall() {
  await waitFor(() => expect(scalping.getStrikes).toHaveBeenCalled())
  await userEvent.click(await screen.findByRole('button', { name: /Buy Call/ }))
}

describe('Scalping predefined stop-loss', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.setItem('scalping.armed', '1')
    feed.data = new Map()
    scalping.getAllUnderlyings.mockResolvedValue({ status: 'success', data: ['NIFTY'] })
    scalping.getExpiry.mockResolvedValue({ status: 'success', data: ['28-OCT-26'] })
    scalping.getStrikes.mockResolvedValue({
      status: 'success',
      underlying: 'NIFTY',
      expiry_date: '28-OCT-26',
      atm_strike: 25000,
      chain: [{ strike: 25000, ce: leg(CE), pe: leg(PE) }],
      fo_exchange: 'NFO',
      underlying_symbol: 'NIFTY',
      underlying_exchange: 'NSE_INDEX',
    })
    scalping.getTracked.mockResolvedValue({ status: 'success', data: [] })
    scalping.getSLStates.mockResolvedValue({ status: 'success', data: [] })
    scalping.placeOrder.mockResolvedValue({ status: 'success', orderid: '1' })
    scalping.saveSL.mockImplementation(async (sl: Record<string, unknown>) => ({
      status: 'success',
      data: sl,
    }))
    trading.getPositions.mockResolvedValue({ status: 'success', data: [] })
    trading.getOrders.mockResolvedValue({ status: 'success', data: { orders: [] } })
    trading.getTrades.mockResolvedValue({ status: 'success', data: [] })
    trading.getMultiQuotes.mockResolvedValue({ status: 'success', results: [] })
  })

  afterEach(() => {
    localStorage.clear()
  })

  it('uses a live tick that is fresh, without reading a quote', async () => {
    feed.data = new Map([[`NFO:${CE}`, { data: { ltp: 150 }, lastUpdate: Date.now() }]])
    mountPage()
    await predefinedStop('10')
    await buyCall()
    await waitFor(() => expect(scalping.saveSL).toHaveBeenCalledTimes(1))
    expect(scalping.saveSL.mock.calls[0][0]).toMatchObject({ entry_price: 150, initial_sl: 140 })
    expect(trading.getQuotes).not.toHaveBeenCalled()
  })

  it('measures from a quote when the feed has no price for the leg', async () => {
    trading.getQuotes.mockResolvedValue({ status: 'success', data: { ltp: 200 } })
    mountPage()
    await predefinedStop('10')
    await buyCall()
    await waitFor(() => expect(scalping.saveSL).toHaveBeenCalledTimes(1))
    expect(trading.getQuotes).toHaveBeenCalledWith('test-key', CE, 'NFO')
    expect(scalping.saveSL.mock.calls[0][0]).toMatchObject({ entry_price: 200, initial_sl: 190 })
  })

  it('measures from a quote, not from a tick older than the freshness window', async () => {
    feed.data = new Map([[`NFO:${CE}`, { data: { ltp: 150 }, lastUpdate: Date.now() - 60_000 }]])
    trading.getQuotes.mockResolvedValue({ status: 'success', data: { ltp: 200 } })
    mountPage()
    await predefinedStop('10')
    await buyCall()
    await waitFor(() => expect(scalping.saveSL).toHaveBeenCalledTimes(1))
    expect(scalping.saveSL.mock.calls[0][0]).toMatchObject({ entry_price: 200, initial_sl: 190 })
  })

  it('says the position has no stop when no price can be had', async () => {
    trading.getQuotes.mockRejectedValue(new Error('no quote'))
    mountPage()
    await predefinedStop('10')
    await buyCall()
    await waitFor(() => expect(scalping.placeOrder).toHaveBeenCalledTimes(1))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        'Order placed, but the predefined stop-loss could not be set because no current price was available. Set it from the position row.',
        undefined,
        { duration: Number.POSITIVE_INFINITY }
      )
    )
    expect(scalping.saveSL).not.toHaveBeenCalled()
  })

  it('does not send an entry while the enabled stop-loss has no usable value', async () => {
    mountPage()
    await predefinedStop('')
    expect(screen.getByRole('alert')).toHaveTextContent('Enter a stop-loss above 0, or untick SL.')
    await buyCall()
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith(
        'Order not sent. Enter a stop-loss above 0, or untick SL.'
      )
    )
    expect(scalping.placeOrder).not.toHaveBeenCalled()
  })
})

describe('Scalping One-Click wording (N-05)', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    localStorage.setItem('scalping.armed', '0')
    scalping.getAllUnderlyings.mockResolvedValue({ status: 'success', data: ['NIFTY'] })
    scalping.getExpiry.mockResolvedValue({ status: 'success', data: [] })
    scalping.getTracked.mockResolvedValue({ status: 'success', data: [] })
    scalping.getSLStates.mockResolvedValue({ status: 'success', data: [] })
    trading.getPositions.mockResolvedValue({ status: 'success', data: [] })
    trading.getOrders.mockResolvedValue({ status: 'success', data: { orders: [] } })
    trading.getTrades.mockResolvedValue({ status: 'success', data: [] })
  })

  it('says One-Click is off, never disarmed', async () => {
    mountPage()
    await userEvent.click(await screen.findByRole('button', { name: /Buy Call/ }))
    expect(toast.error).toHaveBeenCalledWith('One-Click is off. Turn it on to trade.', 'orders')
    expect(scalping.placeOrder).not.toHaveBeenCalled()
  })
})
