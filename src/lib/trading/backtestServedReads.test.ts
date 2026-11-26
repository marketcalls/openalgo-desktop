/**
 * Desktop: a backtest whose strategy reads bars the chart's own cannot answer,
 * end to end on the real compiler and engine.
 *
 * **Why this is not mocked.** From openalgo-script 0.9.0 a backtest learns which
 * reads need bars beyond the chart's by loading the strategy once with a
 * provider that records what it is asked (`servedFor` in `backtestRun.ts`, web
 * 264b3c2a7). The engine decides what it asks from the host it is given, so a
 * test that replaces `load` asserts what the replacement says and not what the
 * engine would ask. That is how a probe with no order route and no symbol or
 * exchange came to pass review: the engine refuses a strategy that places
 * orders on a host with no route before it plans any read (OS6006), so nothing
 * was fetched and the run was refused as on 0.8.1; and with no identity a read
 * of the chart's own instrument named no instrument and a `req.symbol` read no
 * exchange, so neither could match what the run itself asks. So only the
 * network is replaced here: the history, keyed by instrument and interval, and
 * the instrument facts route.
 *
 * The zone and the session are a fixture for what the facts route answers, the
 * regular NSE window the market calendar holds.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'

const post = vi.fn()
vi.mock('@/api/client', () => ({
  apiClient: { post: (...args: unknown[]) => post(...args) },
  fetchCSRFToken: async () => '',
}))

// The run happens on this thread; the worker makes the same engine call.
vi.mock('./backtestWorker', () => ({
  runOnWorker: () => Promise.reject(new Error('no worker here')),
  workersAvailable: () => false,
}))

const { runBacktest } = await import('./backtestRun')
const { clearInstrumentFacts } = await import('./instrumentFacts')

/** A fifteen minute read on a ten minute chart: not a whole multiple, built from five minute bars. */
const QUARTER_HOUR = `version 1
strategy("a quarter hour read")
quarter = req.timeframe("15", close)
if not isNone(quarter) and pos.isFlat
    buy(tag = "long")
`

/** Another instrument's fifteen minute close, which only the host can serve. */
const OTHER_INSTRUMENT = `version 1
strategy("another instrument")
other = req.symbol("RELIANCE", "15", close)
if not isNone(other) and pos.isFlat
    buy(tag = "long")
`

/** A whole multiple of the chart's interval, which the engine folds from the chart's own bars. */
const TWENTY_MINUTES = `version 1
strategy("a whole multiple")
twenty = req.timeframe("20", close)
if not isNone(twenty) and pos.isFlat
    buy(tag = "long")
`

const MIN = 60
/** 09:15 IST on Tuesday 22 September 2026, in the seconds the history API counts. */
const OPEN = Date.UTC(2026, 8, 22, 3, 45) / 1000

/** One session of bars `step` minutes apart from the open, each closing a point higher. */
function session(step: number) {
  const rows = []
  for (let at = 0; at < 375; at += step) {
    const price = 100 + at / step
    rows.push({
      timestamp: OPEN + at * MIN,
      open: price,
      high: price + 1,
      low: price - 1,
      close: price,
      volume: 10,
    })
  }
  return rows
}

interface HistoryAsk {
  symbol: string
  exchange: string
  interval: string
}

/** History answering any instrument at the interval it is asked for, and the facts route. */
function platform() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string) => {
      if (!url.startsWith('/openscript/instrument')) {
        return { ok: false, json: async () => ({}) } as unknown as Response
      }
      return {
        ok: true,
        json: async () => ({
          status: 'success',
          symbol: 'SBIN',
          contractFound: true,
          instrument: {
            exchange: 'NSE',
            timezone: 'Asia/Kolkata',
            tickSize: 0.05,
            lotSize: 1,
            instrumentType: 'equity',
            hasVolume: true,
            hasOpenInterest: false,
            session: { start: '09:15', end: '15:30', days: [1, 2, 3, 4, 5] },
          },
          today: null,
        }),
      } as unknown as Response
    })
  )
  post.mockImplementation(async (url: string, body: HistoryAsk) => {
    if (url !== '/history') return { data: { status: 'error' } }
    const step = { '10m': 10, '5m': 5 }[body.interval]
    if (step === undefined) return { data: { status: 'error', message: 'no such interval' } }
    return { data: { status: 'success', data: session(step) } }
  })
}

function request(source: string) {
  return {
    file: 'reads.oscript',
    source,
    symbol: 'SBIN',
    exchange: 'NSE',
    interval: '10m',
    startDate: '2026-09-22',
    endDate: '2026-09-22',
    apiKey: 'key',
  }
}

/** Every history call the run made, in order. */
function historyAsked(): HistoryAsk[] {
  return post.mock.calls
    .filter(([url]) => url === '/history')
    .map(([, body]) => {
      const { symbol, exchange, interval } = body as HistoryAsk
      return { symbol, exchange, interval }
    })
}

beforeEach(() => {
  vi.clearAllMocks()
  clearInstrumentFacts()
})

afterEach(() => {
  clearInstrumentFacts()
  vi.unstubAllGlobals()
})

describe('a backtest serves the reads its chart bars cannot answer', () => {
  it('fetches a fifteen minute read on a ten minute chart at five minutes, and trades on it', async () => {
    // Review of web 264b3c2a7: refused OS6015 with the probe the web wrote,
    // because the probe was refused before it planned a read and nothing was
    // fetched. The first fifteen minute bucket, 09:15 to 09:30, has closed by
    // the end of the 09:25 bar, so the strategy enters there.
    platform()

    const out = await runBacktest(request(QUARTER_HOUR))

    expect(out.problem).toBeUndefined()
    expect(out.ok).toBe(true)
    expect(historyAsked()).toEqual([
      { symbol: 'SBIN', exchange: 'NSE', interval: '10m' },
      { symbol: 'SBIN', exchange: 'NSE', interval: '5m' },
    ])
    expect(out.trades?.length).toBe(1)
  })

  it('fetches another instrument on the chart exchange, and trades on it', async () => {
    // Refused OS6006 with the probe the web wrote. Given a route and no
    // identity, the probe recorded the read with no exchange while the run
    // asks with the chart's, so the bars fetched were never served: the run
    // passed with no trade and nothing said. Hence a trade, not just ok.
    platform()

    const out = await runBacktest(request(OTHER_INSTRUMENT))

    expect(out.problem).toBeUndefined()
    expect(out.ok).toBe(true)
    expect(historyAsked()).toEqual([
      { symbol: 'SBIN', exchange: 'NSE', interval: '10m' },
      { symbol: 'RELIANCE', exchange: 'NSE', interval: '5m' },
    ])
    expect(out.trades?.length).toBe(1)
  })

  it('fetches nothing beyond the chart for a whole multiple', async () => {
    platform()

    const out = await runBacktest(request(TWENTY_MINUTES))

    expect(out.ok).toBe(true)
    expect(historyAsked()).toEqual([{ symbol: 'SBIN', exchange: 'NSE', interval: '10m' }])
    expect(out.trades?.length).toBe(1)
  })
})
