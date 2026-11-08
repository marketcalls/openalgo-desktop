import { afterEach, describe, expect, it, vi } from 'vitest'
import { currentWebSocketMarketData, mergeOptionChainMarketData } from '@/hooks/useOptionChainLive'
import type { OptionChainResponse } from '@/types/option-chain'
import { MarketDataManager } from './MarketDataManager'

/**
 * The real manager across a reconnect, read by the option chain the way
 * the page reads it (TOOLS-09, gate T11). Before the fix the first tick of
 * a new session merged into the old session's fields and stamped them all
 * with the new epoch, so a depth of 100/101 from before a reconnect came
 * back as the current best bid and ask beside a fresh LTP of 120.
 */

interface Harness {
  handleMessage: (event: MessageEvent) => void
  fallbackMode: boolean
  apiKey: string | null
  fetchMarketDataViaRest: () => Promise<void>
}

const SYMBOL = 'NIFTY27OCT2625000CE'

function send(manager: MarketDataManager, message: unknown) {
  ;(manager as unknown as Harness).handleMessage(
    new MessageEvent('message', { data: JSON.stringify(message) })
  )
}

const authenticated = { type: 'auth', status: 'success' }
const depthTick = {
  type: 'market_data',
  symbol: SYMBOL,
  exchange: 'NFO',
  data: {
    ltp: 100.5,
    depth: { buy: [{ price: 100, quantity: 65 }], sell: [{ price: 101, quantity: 65 }] },
  },
}
const ltpTick = { type: 'market_data', symbol: SYMBOL, exchange: 'NFO', data: { ltp: 120 } }

function chain(): OptionChainResponse {
  const leg = (symbol: string, bid: number, ask: number) => ({
    symbol,
    label: 'ATM',
    ltp: 118.5,
    bid,
    ask,
    bid_qty: 10,
    ask_qty: 10,
    open: 0,
    high: 0,
    low: 0,
    prev_close: 0,
    volume: 0,
    oi: 0,
    lotsize: 65,
    tick_size: 0.05,
  })
  return {
    status: 'success',
    underlying: 'NIFTY',
    underlying_symbol: 'NIFTY',
    underlying_exchange: 'NSE_INDEX',
    underlying_ltp: 25000,
    underlying_prev_close: 25000,
    expiry_date: '27OCT26',
    atm_strike: 25000,
    chain: [{ strike: 25000, ce: leg(SYMBOL, 118, 119), pe: leg('NIFTY27OCT2625000PE', 90, 91) }],
  } as unknown as OptionChainResponse
}

afterEach(() => {
  MarketDataManager.resetInstance()
  vi.unstubAllGlobals()
})

describe('MarketDataManager across sessions', () => {
  it('does not carry an old depth into a new session', () => {
    const manager = MarketDataManager.getInstance()
    manager.subscribe(SYMBOL, 'NFO', 'Depth', () => {})
    send(manager, authenticated)
    send(manager, depthTick)
    expect(manager.getCachedData(SYMBOL, 'NFO')?.data.depth?.buy[0].price).toBe(100)

    // The socket drops and comes back: a new authentication, then a tick
    // that carries only an LTP.
    send(manager, authenticated)
    send(manager, ltpTick)
    const cached = manager.getCachedData(SYMBOL, 'NFO')
    expect(cached?.data.ltp).toBe(120)
    expect(cached?.data.depth).toBeUndefined()

    // What the option chain prices with: the current LTP, and the polled
    // bid and ask rather than the old book.
    const epoch = manager.getState().connectionEpoch
    const current = currentWebSocketMarketData(manager.getAllCachedData(), true, epoch)
    const merged = mergeOptionChainMarketData(chain(), 'NFO', current, 0)
    expect(merged.chain[0].ce).toMatchObject({ ltp: 120, bid: 118, ask: 119 })
  })

  it('keeps merging within one session', () => {
    const manager = MarketDataManager.getInstance()
    manager.subscribe(SYMBOL, 'NFO', 'Depth', () => {})
    send(manager, authenticated)
    send(manager, depthTick)
    send(manager, ltpTick)
    const cached = manager.getCachedData(SYMBOL, 'NFO')
    expect(cached?.data.ltp).toBe(120)
    expect(cached?.data.depth?.sell[0].price).toBe(101)
  })

  // Audit C03: the server leaves out what the broker did not send, so a
  // frame without a field keeps the field's value from earlier in the
  // session, and a field sent as 0 is a real 0 (no volume yet, a close the
  // broker sent), never mistaken for a missing one.
  it('merges a field by its presence, a sent zero included', () => {
    const manager = MarketDataManager.getInstance()
    manager.subscribe(SYMBOL, 'NFO', 'Quote', () => {})
    send(manager, authenticated)
    const quote = (data: Record<string, unknown>) => ({
      type: 'market_data',
      symbol: SYMBOL,
      exchange: 'NFO',
      data,
    })
    send(manager, quote({ ltp: 118.5, open: 110, close: 105, volume: 1200 }))
    send(manager, quote({ ltp: 119 }))
    expect(manager.getCachedData(SYMBOL, 'NFO')?.data).toMatchObject({
      ltp: 119,
      open: 110,
      close: 105,
      volume: 1200,
    })
    send(manager, quote({ ltp: 119.5, volume: 0 }))
    expect(manager.getCachedData(SYMBOL, 'NFO')?.data).toMatchObject({
      ltp: 119.5,
      open: 110,
      volume: 0,
    })
  })

  it('drops the book once no Depth subscription is left', () => {
    const manager = MarketDataManager.getInstance()
    const leaveDepth = manager.subscribe(SYMBOL, 'NFO', 'Depth', () => {})
    manager.subscribe(SYMBOL, 'NFO', 'LTP', () => {})
    send(manager, authenticated)
    send(manager, depthTick)
    leaveDepth()
    send(manager, ltpTick)
    const cached = manager.getCachedData(SYMBOL, 'NFO')
    expect(cached?.data.ltp).toBe(120)
    expect(cached?.data.depth).toBeUndefined()
  })

  it('does not hand a session depth to a later REST answer', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() =>
        Promise.resolve(
          new Response(
            JSON.stringify({
              status: 'success',
              results: [{ symbol: SYMBOL, exchange: 'NFO', data: { ltp: 99, bid: 98, ask: 99.5 } }],
            })
          )
        )
      )
    )
    const manager = MarketDataManager.getInstance()
    manager.subscribe(SYMBOL, 'NFO', 'Depth', () => {})
    send(manager, authenticated)
    send(manager, depthTick)
    const harness = manager as unknown as Harness
    harness.fallbackMode = true
    harness.apiKey = 'key'
    await harness.fetchMarketDataViaRest()
    const cached = manager.getCachedData(SYMBOL, 'NFO')
    expect(cached).toMatchObject({ updateSource: 'rest', data: { ltp: 99, bid_price: 98 } })
    expect(cached?.data.depth).toBeUndefined()
  })
})
