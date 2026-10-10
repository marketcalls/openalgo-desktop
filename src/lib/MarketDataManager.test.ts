import { afterEach, describe, expect, it, vi } from 'vitest'
import { MarketDataManager } from './MarketDataManager'

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((done) => {
    resolve = done
  })
  return { promise, resolve }
}

interface ManagerHarness {
  fallbackMode: boolean
  apiKey: string | null
  enableFallbackMode: () => Promise<void>
  fetchMarketDataViaRest: () => Promise<void>
  disableFallbackMode: () => void
  startFallbackPolling: () => void
  handleMessage: (event: MessageEvent) => void
}

describe('MarketDataManager fallback sequencing', () => {
  afterEach(() => {
    MarketDataManager.resetInstance()
    vi.unstubAllGlobals()
  })

  it('does not let an old REST fallback session replace a newer WebSocket tick', async () => {
    const response = deferred<Response>()
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        if (url === '/api/v1/multiquotes') return response.promise
        if (url === '/auth/csrf-token') {
          return Promise.resolve(
            new Response(JSON.stringify({ csrf_token: 'csrf' }), { status: 200 })
          )
        }
        if (url === '/api/websocket/apikey') {
          return Promise.resolve(
            new Response(JSON.stringify({ status: 'success', api_key: 'key' }), { status: 200 })
          )
        }
        return Promise.reject(new Error(`Unexpected fetch: ${url}`))
      })
    )

    const manager = MarketDataManager.getInstance()
    const received: number[] = []
    manager.subscribe('NIFTY13AUG2624600CE', 'NFO', 'Depth', (update) => {
      if (update.data.ltp !== undefined) received.push(update.data.ltp)
    })

    const harness = manager as unknown as ManagerHarness
    harness.fallbackMode = true
    harness.apiKey = 'key'
    const pendingFallback = harness.fetchMarketDataViaRest()

    harness.handleMessage(
      new MessageEvent('message', {
        data: JSON.stringify({
          type: 'market_data',
          symbol: 'NIFTY13AUG2624600CE',
          exchange: 'NFO',
          data: { ltp: 200 },
        }),
      })
    )
    harness.disableFallbackMode()
    const startFallbackPolling = vi
      .spyOn(harness, 'startFallbackPolling')
      .mockImplementation(() => {})
    await harness.enableFallbackMode()
    expect(startFallbackPolling).toHaveBeenCalledOnce()
    response.resolve(
      new Response(
        JSON.stringify({
          status: 'success',
          results: [
            {
              symbol: 'NIFTY13AUG2624600CE',
              exchange: 'NFO',
              data: { ltp: 100 },
            },
          ],
        })
      )
    )
    await pendingFallback

    expect(received).toEqual([200])
    expect(manager.getCachedData('NIFTY13AUG2624600CE', 'NFO')).toMatchObject({
      data: { ltp: 200 },
      updateSource: 'websocket',
    })
  })
})

interface RestHarness extends ManagerHarness {
  socket: unknown
  connectionState: string
}

/** Answers the CSRF and API-key calls and hands each multiquotes call to `multiquotes`. */
function stubFetch(multiquotes: (init: RequestInit | undefined) => Promise<Response>) {
  const calls: unknown[] = []
  vi.stubGlobal(
    'fetch',
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input)
      if (url === '/api/v1/multiquotes') {
        calls.push(JSON.parse(String(init?.body)))
        return multiquotes(init)
      }
      if (url === '/auth/csrf-token') {
        return Promise.resolve(
          new Response(JSON.stringify({ csrf_token: 'csrf' }), { status: 200 })
        )
      }
      if (url === '/api/websocket/apikey') {
        return Promise.resolve(
          new Response(JSON.stringify({ status: 'success', api_key: 'key' }), { status: 200 })
        )
      }
      return Promise.reject(new Error(`Unexpected fetch: ${url}`))
    })
  )
  return calls
}

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status })
}

/** The manager in REST fallback, without starting the 5 s poll. */
function inFallback(): { manager: MarketDataManager; harness: RestHarness } {
  const manager = MarketDataManager.getInstance()
  const harness = manager as unknown as RestHarness
  harness.fallbackMode = true
  harness.apiKey = 'key'
  vi.spyOn(harness, 'startFallbackPolling').mockImplementation(() => {})
  return { manager, harness }
}

describe('MarketDataManager REST fallback answers', () => {
  afterEach(() => {
    MarketDataManager.resetInstance()
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  // The multiquotes answer lists unknown symbols first as {symbol, exchange,
  // error} with no data, and a broker's per-symbol failure the same way. One
  // such row used to throw and drop every good quote in the answer.
  it('applies every quoted row and skips rows that carry an error', async () => {
    stubFetch(() =>
      Promise.resolve(
        json({
          status: 'success',
          results: [
            { symbol: 'NOPE1', exchange: 'NSE', error: 'Symbol NOPE1 not found in exchange NSE' },
            { symbol: 'SBIN', exchange: 'NSE', data: { ltp: 801.5, prev_close: 790 } },
            { symbol: 'NOPE2', exchange: 'NSE', error: 'Symbol NOPE2 not found in exchange NSE' },
            { symbol: 'INFY', exchange: 'NSE', error: 'No quote data available' },
            { symbol: 'RELIANCE', exchange: 'NSE', data: { ltp: 2950 } },
            { symbol: 'NOPE3', exchange: 'NSE', error: 'Symbol NOPE3 not found in exchange NSE' },
          ],
        })
      )
    )
    const { manager, harness } = inFallback()
    const seen: Record<string, number[]> = {}
    for (const symbol of ['SBIN', 'RELIANCE', 'INFY', 'NOPE1', 'NOPE2', 'NOPE3']) {
      manager.subscribe(symbol, 'NSE', 'LTP', (update) => {
        if (update.data.ltp !== undefined) {
          seen[symbol] = [...(seen[symbol] ?? []), update.data.ltp]
        }
      })
    }

    await harness.fetchMarketDataViaRest()

    expect(seen).toEqual({ SBIN: [801.5], RELIANCE: [2950] })
    expect(manager.getCachedData('SBIN', 'NSE')).toMatchObject({
      data: { ltp: 801.5, close: 790 },
      updateSource: 'rest',
    })
    expect(manager.getCachedData('RELIANCE', 'NSE')?.lastUpdate).toEqual(expect.any(Number))
    for (const failed of ['INFY', 'NOPE1', 'NOPE2', 'NOPE3']) {
      expect(manager.getCachedData(failed, 'NSE')?.lastUpdate, failed).toBeUndefined()
    }
  })

  it('leaves the cache alone when every symbol is refused', async () => {
    stubFetch(() =>
      Promise.resolve(
        json(
          {
            status: 'error',
            message: 'Symbol NOPE not found in exchange NSE',
            invalid_symbols: [
              { symbol: 'NOPE', exchange: 'NSE', error: 'Symbol NOPE not found in exchange NSE' },
            ],
          },
          400
        )
      )
    )
    const { manager, harness } = inFallback()
    const callback = vi.fn()
    manager.subscribe('NOPE', 'NSE', 'LTP', callback)

    await harness.fetchMarketDataViaRest()

    expect(callback).not.toHaveBeenCalled()
    expect(manager.getCachedData('NOPE', 'NSE')?.lastUpdate).toBeUndefined()
  })

  // Polls every 5 s with no guard overlapped when a call took longer than
  // that, and the older answer, arriving last, overwrote the newer one.
  it('keeps one REST call in flight, so an older answer cannot land after a newer one', async () => {
    const pending: Array<Deferred<Response>> = []
    const calls = stubFetch(() => {
      const next = deferred<Response>()
      pending.push(next)
      return next.promise
    })
    const { manager, harness } = inFallback()
    const ltps: number[] = []
    manager.subscribe('SBIN', 'NSE', 'Quote', (update) => {
      if (update.data.ltp !== undefined) ltps.push(update.data.ltp)
    })
    const quote = (ltp: number) =>
      json({ status: 'success', results: [{ symbol: 'SBIN', exchange: 'NSE', data: { ltp } }] })

    const first = harness.fetchMarketDataViaRest()
    // The next poll tick, while the first call is still running.
    const second = harness.fetchMarketDataViaRest()
    await Promise.resolve()
    expect(calls).toHaveLength(1)

    // Answer whatever went out, newest first, so the oldest answer lands last.
    for (let i = pending.length - 1; i >= 0; i--) pending[i].resolve(quote(100 + i))
    await Promise.all([first, second])
    expect(manager.getCachedData('SBIN', 'NSE')?.data.ltp).toBe(100)

    // Once it has answered, the next poll goes out and its answer is shown.
    const third = harness.fetchMarketDataViaRest()
    await Promise.resolve()
    expect(calls).toHaveLength(2)
    pending[pending.length - 1].resolve(quote(205))
    await third
    expect(ltps).toEqual([100, 205])
    expect(manager.getCachedData('SBIN', 'NSE')?.data.ltp).toBe(205)
  })

  it('abandons a call that never answers when the fallback session ends', async () => {
    let aborted = false
    stubFetch(
      (init) =>
        new Promise<Response>((_resolve, reject) => {
          // Never answers: only an abort releases it.
          init?.signal?.addEventListener('abort', () => {
            aborted = true
            reject(new DOMException('aborted', 'AbortError'))
          })
        })
    )
    const { manager, harness } = inFallback()
    manager.subscribe('SBIN', 'NSE', 'LTP', () => {})
    const stuck = harness.fetchMarketDataViaRest()
    await Promise.resolve()
    harness.disableFallbackMode()
    await stuck
    expect(aborted).toBe(true)

    // The next fallback session polls at once instead of waiting behind it.
    harness.fallbackMode = true
    const calls = stubFetch(() =>
      Promise.resolve(
        json({
          status: 'success',
          results: [{ symbol: 'SBIN', exchange: 'NSE', data: { ltp: 1 } }],
        })
      )
    )
    await harness.fetchMarketDataViaRest()
    expect(calls).toHaveLength(1)
    expect(manager.getCachedData('SBIN', 'NSE')?.data.ltp).toBe(1)
  })
})

describe('MarketDataManager unsubscribe', () => {
  afterEach(() => {
    MarketDataManager.resetInstance()
  })

  function connected() {
    const manager = MarketDataManager.getInstance()
    const harness = manager as unknown as RestHarness
    const sent: Array<Record<string, unknown>> = []
    harness.socket = {
      readyState: WebSocket.OPEN,
      send: (frame: string) => sent.push(JSON.parse(frame)),
      close: () => {},
    }
    harness.connectionState = 'authenticated'
    return { manager, sent }
  }

  const frame = (mode: string) => ({
    action: 'unsubscribe',
    symbols: [{ symbol: 'SBIN', exchange: 'NSE' }],
    mode,
  })

  // The feed keeps one owner per instrument and mode, and reads an
  // unsubscribe with no mode as Quote. Leaving LTP or Depth without naming
  // the mode left that owner on the server until the socket closed.
  it('releases each mode it leaves, naming the mode, even while another mode stays', () => {
    const { manager, sent } = connected()
    const offLtp = manager.subscribe('SBIN', 'NSE', 'LTP', () => {})
    const offDepth = manager.subscribe('SBIN', 'NSE', 'Depth', () => {})
    sent.length = 0

    offLtp()
    expect(sent).toEqual([frame('LTP')])
    expect(manager.getCachedData('SBIN', 'NSE')).toBeDefined()

    offDepth()
    expect(sent).toEqual([frame('LTP'), frame('Depth')])
    expect(manager.getCachedData('SBIN', 'NSE')).toBeUndefined()
  })

  it('sends nothing while another consumer still holds the same mode', () => {
    const { manager, sent } = connected()
    const offA = manager.subscribe('SBIN', 'NSE', 'Quote', () => {})
    const offB = manager.subscribe('SBIN', 'NSE', 'Quote', () => {})
    sent.length = 0

    offA()
    expect(sent).toEqual([])
    offB()
    expect(sent).toEqual([frame('Quote')])
  })
})
