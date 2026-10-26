/**
 * docs-43: an order the ticket posts straight to the API (option chain,
 * holdings, market depth) while the API key's order mode is Semi-Auto is
 * queued in Action Center. The server answers success with no order id;
 * the ticket showed that answer as an error and stayed open.
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'
import { tradingApi } from '@/api/trading'
import { WAITING_FOR_APPROVAL } from '@/lib/trading/semiAutoQueue'
import { render, screen, userEvent, waitFor } from '@/test/test-utils'
import { showToast } from '@/utils/toast'
import { PlaceOrderDialog } from './PlaceOrderDialog'

vi.mock('@/api/trading', async () => {
  const actual = await vi.importActual<typeof import('@/api/trading')>('@/api/trading')
  return { ...actual, tradingApi: { ...actual.tradingApi, placeOrder: vi.fn() } }
})

vi.mock('@/utils/toast', () => ({
  showToast: { error: vi.fn(), success: vi.fn(), info: vi.fn() },
}))

vi.mock('@/stores/authStore', () => ({
  useAuthStore: () => ({ apiKey: 'test-api-key' }),
}))

vi.mock('@/hooks/useLiveQuote', () => ({
  useLiveQuote: () => ({
    data: { ltp: 100, close: 99, change: 1, changePercent: 1.01 },
    isLive: false,
    isConnected: false,
    isLoading: false,
    isPaused: false,
    isFallbackMode: false,
    dataSource: 'none',
    refresh: async () => {},
  }),
}))

const placeOrder = vi.mocked(tradingApi.placeOrder)

const sbin = {
  open: true,
  symbol: 'SBIN',
  exchange: 'NSE',
  action: 'BUY' as const,
  quantity: 1,
  product: 'MIS' as const,
  priceType: 'MARKET' as const,
}

describe('PlaceOrderDialog in Semi-Auto mode', () => {
  beforeEach(() => placeOrder.mockReset())

  it('says the order waits in Action Center and closes, as placed', async () => {
    placeOrder.mockResolvedValue({
      status: 'success',
      message: 'Order queued for approval in Action Center',
      mode: 'semi_auto',
      pending_order_id: 7,
    } as never)
    const onOpenChange = vi.fn()
    const onError = vi.fn()
    render(<PlaceOrderDialog {...sbin} onOpenChange={onOpenChange} onError={onError} />)
    await userEvent.click(screen.getByRole('button', { name: 'Place BUY Order' }))
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false))
    expect(showToast.info).toHaveBeenCalledWith(WAITING_FOR_APPROVAL, 'orders')
    expect(showToast.error).not.toHaveBeenCalled()
    expect(onError).not.toHaveBeenCalled()
  })
})
