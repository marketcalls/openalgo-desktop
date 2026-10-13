/**
 * The order book: what a trader can act on, and what each action sends.
 *
 * Only an open order offers Cancel and Modify; Cancel All asks first; a
 * modification sends the fields its price type uses and no others (an SL-M
 * with a price, or a LIMIT with a trigger, is refused or misread by some
 * brokers).
 */

import { beforeEach, describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen, userEvent, waitFor, within } from '@/test/test-utils'
import type { Order } from '@/types/trading'

const mocks = vi.hoisted(() => ({
  getOrders: vi.fn(),
  cancelOrder: vi.fn(),
  cancelAllOrders: vi.fn(),
  modifyOrder: vi.fn(),
  getQuotes: vi.fn(),
}))

vi.mock('@/api/trading', () => ({ tradingApi: mocks }))
vi.mock('@/stores/authStore', () => ({
  useAuthStore: () => ({ apiKey: 'test-api-key', user: { broker: 'zerodha' } }),
}))
vi.mock('@/stores/themeStore', () => ({ onModeChange: () => () => {} }))
const eventRefresh = vi.hoisted(() => vi.fn())
vi.mock('@/hooks/useOrderEventRefresh', () => ({
  useOrderEventRefresh: (...args: unknown[]) => eventRefresh(...args),
}))
vi.mock('@/hooks/useSupportedExchanges', () => ({
  useSupportedExchanges: () => ({ isCrypto: false }),
}))
vi.mock('@/components/trading/GttTab', () => ({ default: () => null }))

import OrderBook from './OrderBook'

const order = (o: Partial<Order>): Order => ({
  symbol: 'SBIN',
  exchange: 'NSE',
  action: 'BUY',
  quantity: 10,
  price: 800,
  trigger_price: 0,
  pricetype: 'LIMIT',
  product: 'MIS',
  orderid: 'O1',
  order_status: 'open',
  timestamp: '10-Oct-2026 10:00:00',
  ...o,
})

const OPEN_LIMIT = order({})
const DONE_MARKET = order({
  symbol: 'INFY',
  orderid: 'O2',
  pricetype: 'MARKET',
  price: 0,
  order_status: 'complete',
})
const OPEN_SLM = order({
  symbol: 'RELIANCE',
  orderid: 'O3',
  action: 'SELL',
  pricetype: 'SL-M',
  price: 0,
  trigger_price: 2900,
})

async function renderBook(orders: Order[]) {
  mocks.getOrders.mockResolvedValue({
    status: 'success',
    data: {
      orders,
      statistics: {
        total_buy_orders: 0,
        total_sell_orders: 0,
        total_completed_orders: 0,
        total_open_orders: 0,
        total_rejected_orders: 0,
      },
    },
  })
  render(<OrderBook />)
  await screen.findByText('SBIN')
}

describe('Order book', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mocks.getQuotes.mockResolvedValue({ status: 'error' })
    mocks.cancelOrder.mockResolvedValue({ status: 'success' })
    mocks.cancelAllOrders.mockResolvedValue({ status: 'success', message: 'All orders cancelled' })
    mocks.modifyOrder.mockResolvedValue({ status: 'success' })
  })

  it("refreshes on the broker's later status for a resting order (order_update)", async () => {
    await renderBook([OPEN_LIMIT])
    expect(eventRefresh).toHaveBeenCalledWith(
      expect.any(Function),
      expect.objectContaining({ events: expect.arrayContaining(['order_update']) })
    )
  })

  it('offers Cancel and Modify on open orders only', async () => {
    await renderBook([OPEN_LIMIT, DONE_MARKET, OPEN_SLM])

    expect(screen.getByRole('button', { name: 'Cancel order O1' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Cancel order O3' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Cancel order O2' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Modify order for INFY' })).not.toBeInTheDocument()
  })

  it('cancels the order whose button was pressed', async () => {
    await renderBook([OPEN_LIMIT, OPEN_SLM])

    fireEvent.click(screen.getByRole('button', { name: 'Cancel order O3' }))

    await waitFor(() => expect(mocks.cancelOrder).toHaveBeenCalledWith('O3'))
    expect(mocks.cancelOrder).toHaveBeenCalledTimes(1)
  })

  it('asks before cancelling all orders, and Keep Orders sends nothing', async () => {
    await renderBook([OPEN_LIMIT, OPEN_SLM])
    const user = userEvent.setup()

    await user.click(screen.getByRole('button', { name: /Cancel All/ }))
    const ask = await screen.findByRole('dialog', { name: 'Cancel All Orders?' })
    await user.click(within(ask).getByRole('button', { name: 'Keep Orders' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(mocks.cancelAllOrders).not.toHaveBeenCalled()

    await user.click(screen.getByRole('button', { name: /Cancel All/ }))
    const again = await screen.findByRole('dialog', { name: 'Cancel All Orders?' })
    await user.click(within(again).getByRole('button', { name: 'Cancel All' }))
    await waitFor(() => expect(mocks.cancelAllOrders).toHaveBeenCalledTimes(1))
  })

  it('modifies an SL-M order with its trigger price and no price', async () => {
    await renderBook([OPEN_LIMIT, OPEN_SLM])

    fireEvent.click(screen.getByRole('button', { name: 'Modify order for RELIANCE' }))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).queryByLabelText('Price')).not.toBeInTheDocument()
    fireEvent.change(within(dialog).getByLabelText(/Trigger/), { target: { value: '2895.5' } })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Modify Order' }))

    await waitFor(() => expect(mocks.modifyOrder).toHaveBeenCalledTimes(1))
    const [orderid, body] = mocks.modifyOrder.mock.calls[0]
    expect(orderid).toBe('O3')
    expect(body).toMatchObject({
      symbol: 'RELIANCE',
      action: 'SELL',
      pricetype: 'SL-M',
      quantity: 10,
      trigger_price: 2895.5,
    })
    expect(body).not.toHaveProperty('price')
  })

  it('modifies a LIMIT order with its price and no trigger price', async () => {
    await renderBook([OPEN_LIMIT])

    fireEvent.click(screen.getByRole('button', { name: 'Modify order for SBIN' }))
    const dialog = await screen.findByRole('dialog')
    expect(within(dialog).queryByLabelText(/Trigger/)).not.toBeInTheDocument()
    fireEvent.change(within(dialog).getByLabelText('Price'), { target: { value: '799.05' } })
    fireEvent.click(within(dialog).getByRole('button', { name: 'Modify Order' }))

    await waitFor(() => expect(mocks.modifyOrder).toHaveBeenCalledTimes(1))
    const [, body] = mocks.modifyOrder.mock.calls[0]
    expect(body).toMatchObject({ pricetype: 'LIMIT', price: 799.05, quantity: 10 })
    expect(body).not.toHaveProperty('trigger_price')
  })

  it('says so when the order book cannot be loaded', async () => {
    mocks.getOrders.mockRejectedValue(new Error('network'))
    render(<OrderBook />)

    expect(await screen.findByText('Failed to fetch orders')).toBeInTheDocument()
  })
})
