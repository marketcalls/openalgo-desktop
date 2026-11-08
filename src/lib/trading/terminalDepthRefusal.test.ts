import { createChart, type SeriesApi, type WsControlMessage } from 'openalgo-charts'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { DEPTH_UNAVAILABLE } from './depthRefusal'
import { type SymbolView, TradingTerminal } from './terminal'

// Desktop: the terminal on a broker that refuses market depth (IndMoney).
// Only the feed is replaced; the terminal's own subscribe path runs.
type TerminalState = {
  chart: ReturnType<typeof createChart>
  price: SeriesApi
  volume: SeriesApi
  sym: SymbolView
  interval: string
  rawBars: never[]
  ws: unknown
  connectLive(): void
  followQuoteIfDepthRefused(m: WsControlMessage): void
}

const terminals: TradingTerminal[] = []
beforeEach(() => {
  vi.useFakeTimers()
  // The chart paints into a stub canvas; nothing else is replaced.
  const context = new Proxy(
    {
      measureText: (text: string) => ({ width: text.length * 7 }),
      createLinearGradient: () => ({ addColorStop() {} }),
      getImageData: () => ({ data: new Uint8ClampedArray([0, 0, 0, 255]) }),
    },
    { get: (target, key) => target[key as keyof typeof target] ?? (() => {}) }
  )
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(
    context as unknown as CanvasRenderingContext2D
  )
})
afterEach(() => {
  for (const t of terminals.splice(0)) t.destroy()
  vi.clearAllTimers()
  vi.useRealTimers()
  vi.restoreAllMocks()
})

function mount() {
  const toasts: string[] = []
  const container = document.createElement('div')
  const terminal = new TradingTerminal({
    apiKey: 'test',
    wsUrl: 'ws://test.invalid',
    container,
    legendEl: document.createElement('div'),
    getTheme: () => ({ mode: 'dark', appMode: 'live' }),
    callbacks: {
      onReady() {},
      onToast: (msg: string) => toasts.push(msg),
      onWsState() {},
      onSymbolLoaded() {},
      onLtp() {},
    },
  })
  terminals.push(terminal)
  const state = terminal as unknown as TerminalState
  state.chart = createChart(container, { shortcuts: false, timeNavigator: false })
  state.price = state.chart.addSeries('candlestick')
  state.volume = state.chart.addSeries('histogram', { priceScaleId: '' })
  state.sym = {
    symbol: 'SBIN',
    exchange: 'NSE',
    name: 'State Bank of India',
    lotsize: 1,
    lots: false,
    tick: 0.05,
    freezeQty: 0,
    quoteOnly: false,
    productOptions: ['MIS', 'CNC'],
    product: 'MIS',
  } as SymbolView
  state.interval = '1m'
  state.rawBars = []
  const subscribe = vi.fn()
  const unsubscribe = vi.fn()
  state.ws = { onLtp: () => () => {}, onDepth: () => () => {}, subscribe, unsubscribe }
  return { state, subscribe, unsubscribe, toasts }
}

const refusal = {
  type: 'subscribe',
  status: 'partial',
  subscriptions: [{ symbol: 'SBIN', exchange: 'NSE', status: 'error', message: 'x' }],
}

describe('the terminal on a broker without market depth', () => {
  it('follows a refused Depth at Quote once, and stays there for the symbol', () => {
    const { state, subscribe, unsubscribe, toasts } = mount()
    state.connectLive()
    expect(subscribe.mock.calls).toEqual([['Depth', 'SBIN', 'NSE', 5]])

    state.followQuoteIfDepthRefused(refusal)
    state.followQuoteIfDepthRefused(refusal)
    expect(unsubscribe.mock.calls).toEqual([['Depth', 'SBIN', 'NSE']])
    expect(subscribe.mock.calls.at(-1)).toEqual(['Quote', 'SBIN', 'NSE'])
    expect(subscribe).toHaveBeenCalledTimes(2)
    expect(toasts).toEqual([DEPTH_UNAVAILABLE])

    state.connectLive()
    expect(subscribe.mock.calls.at(-1)).toEqual(['Quote', 'SBIN', 'NSE'])
  })

  it('ignores answers for other symbols and successful ones', () => {
    const { state, subscribe, toasts } = mount()
    state.connectLive()
    state.followQuoteIfDepthRefused({
      ...refusal,
      subscriptions: [{ symbol: 'TCS', exchange: 'NSE', status: 'error' }],
    })
    state.followQuoteIfDepthRefused({ type: 'subscribe', status: 'success', subscriptions: [] })
    expect(subscribe).toHaveBeenCalledTimes(1)
    expect(toasts).toEqual([])
  })
})
