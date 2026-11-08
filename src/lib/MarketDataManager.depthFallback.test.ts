import { afterEach, describe, expect, it } from 'vitest'
import { MarketDataManager } from './MarketDataManager'

/**
 * Desktop: a broker without market depth (IndMoney, as on the web) refuses
 * a Depth subscription. The manager keeps the symbol at Quote for its Depth
 * consumers, once, hands them the quote with an empty book, says the book
 * is not available, and forgets it when the broker changes.
 */

interface Harness {
  handleMessage: (event: MessageEvent) => void
  socket: { readyState: number; send: (m: string) => void; close: () => void } | null
  connectionState: string
}

const SYMBOL = 'SBIN'

function harness(manager: MarketDataManager): { sent: Array<Record<string, unknown>> } {
  const sent: Array<Record<string, unknown>> = []
  const h = manager as unknown as Harness
  h.socket = {
    readyState: WebSocket.OPEN,
    send: (m) => sent.push(JSON.parse(m)),
    close: () => {},
  }
  return { sent }
}

function send(manager: MarketDataManager, message: unknown) {
  ;(manager as unknown as Harness).handleMessage(
    new MessageEvent('message', { data: JSON.stringify(message) })
  )
}

function authenticate(manager: MarketDataManager, broker: string) {
  ;(manager as unknown as Harness).connectionState = 'authenticated'
  send(manager, { type: 'auth', status: 'success', broker })
}

function refuse(manager: MarketDataManager, requestId: unknown) {
  send(manager, {
    type: 'subscribe',
    status: 'error',
    subscriptions: [
      {
        symbol: SYMBOL,
        exchange: 'NSE',
        status: 'error',
        message: 'Depth is not available',
        broker: 'indmoney',
      },
    ],
    request_id: requestId,
  })
}

afterEach(() => {
  MarketDataManager.resetInstance()
})

describe('MarketDataManager without market depth', () => {
  it('keeps a refused Depth at Quote once and gives Depth consumers the quote', () => {
    const manager = MarketDataManager.getInstance()
    const { sent } = harness(manager)
    authenticate(manager, 'indmoney')
    const seen: unknown[] = []
    manager.subscribe(SYMBOL, 'NSE', 'Depth', (d) => seen.push(d.data))
    const depthAsk = sent.find((m) => m.mode === 'Depth')
    expect(depthAsk?.request_id).toBeTruthy()

    refuse(manager, depthAsk?.request_id)
    refuse(manager, depthAsk?.request_id)
    const quoteAsks = sent.filter((m) => m.action === 'subscribe' && m.mode === 'Quote')
    expect(quoteAsks).toHaveLength(1)
    expect(manager.isDepthUnavailable(SYMBOL, 'NSE')).toBe(true)

    send(manager, {
      type: 'market_data',
      symbol: SYMBOL,
      exchange: 'NSE',
      mode: 2,
      data: { ltp: 801.5, bid_price: 801.4, bid_qty: 120, ask_price: 801.6, ask_qty: 90 },
    })
    expect(manager.getCachedData(SYMBOL, 'NSE')?.data).toMatchObject({
      ltp: 801.5,
      bid_price: 801.4,
      bid_size: 120,
      ask_size: 90,
      depth: { buy: [], sell: [] },
    })
    expect(seen).toHaveLength(1)
  })

  it('asks a later Depth at Quote directly, and releases that Quote with the last Depth', () => {
    const manager = MarketDataManager.getInstance()
    const { sent } = harness(manager)
    authenticate(manager, 'indmoney')
    const leave = manager.subscribe(SYMBOL, 'NSE', 'Depth', () => {})
    refuse(manager, sent.find((m) => m.mode === 'Depth')?.request_id)
    sent.length = 0

    // A reconnect: the same broker, every subscription sent again.
    authenticate(manager, 'indmoney')
    expect(sent.filter((m) => m.mode === 'Depth')).toHaveLength(0)
    expect(sent.filter((m) => m.mode === 'Quote' && m.action === 'subscribe')).toHaveLength(1)

    sent.length = 0
    leave()
    expect(sent.filter((m) => m.action === 'unsubscribe').map((m) => m.mode)).toContain('Quote')
  })

  it('forgets the refusals when the broker changes', () => {
    const manager = MarketDataManager.getInstance()
    const { sent } = harness(manager)
    authenticate(manager, 'indmoney')
    let changes = 0
    manager.onDepthAvailabilityChange(() => {
      changes += 1
    })
    manager.subscribe(SYMBOL, 'NSE', 'Depth', () => {})
    refuse(manager, sent.find((m) => m.mode === 'Depth')?.request_id)
    expect(manager.isDepthUnavailable(SYMBOL, 'NSE')).toBe(true)
    sent.length = 0

    authenticate(manager, 'zerodha')
    expect(manager.isDepthUnavailable(SYMBOL, 'NSE')).toBe(false)
    expect(changes).toBe(2)
    expect(sent.filter((m) => m.mode === 'Depth')).toHaveLength(1)
  })

  it('treats an INVALID_MODE error for a Depth request as the same refusal', () => {
    const manager = MarketDataManager.getInstance()
    const { sent } = harness(manager)
    authenticate(manager, 'indmoney')
    manager.subscribe(SYMBOL, 'NSE', 'Depth', () => {})
    const id = sent.find((m) => m.mode === 'Depth')?.request_id
    send(manager, { status: 'error', code: 'INVALID_MODE', message: 'no depth', request_id: id })
    expect(manager.isDepthUnavailable(SYMBOL, 'NSE')).toBe(true)
    expect(manager.getState().error).toBeNull()
  })
})
