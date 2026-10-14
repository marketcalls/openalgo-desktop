import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useAuthStore } from './authStore'
import { useBrokerStore } from './brokerStore'

const ZERODHA = {
  broker_name: 'zerodha',
  broker_type: 'IN_stock' as const,
  supported_exchanges: ['NSE', 'NFO'],
  leverage_config: false,
}

function answer(capabilities: unknown): Response {
  return new Response(JSON.stringify({ status: 'success', data: capabilities }), { status: 200 })
}

function deferredFetch() {
  let resolve!: (r: Response) => void
  const promise = new Promise<Response>((done) => {
    resolve = done
  })
  vi.stubGlobal(
    'fetch',
    vi.fn(() => promise)
  )
  return resolve
}

describe('broker capabilities store', () => {
  beforeEach(() => {
    useBrokerStore.setState({ capabilities: null, isLoaded: false })
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('loads the capabilities the server reports', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() => Promise.resolve(answer(ZERODHA)))
    )
    await useBrokerStore.getState().fetchCapabilities()
    expect(useBrokerStore.getState()).toMatchObject({ capabilities: ZERODHA, isLoaded: true })
  })

  // A fetch started for one session that answers after the store was
  // cleared (logout, a broker change) used to put that session's broker
  // back, so menus were gated on a broker no longer connected.
  it('discards an answer that arrives after the capabilities were cleared', async () => {
    const resolve = deferredFetch()
    const pending = useBrokerStore.getState().fetchCapabilities()

    useBrokerStore.getState().clearCapabilities()
    resolve(answer(ZERODHA))
    await pending

    expect(useBrokerStore.getState()).toMatchObject({ capabilities: null, isLoaded: false })
  })

  it('keeps the answer of a fetch started after the clear', async () => {
    useBrokerStore.getState().clearCapabilities()
    const resolve = deferredFetch()
    const pending = useBrokerStore.getState().fetchCapabilities()
    resolve(answer(ZERODHA))
    await pending

    expect(useBrokerStore.getState()).toMatchObject({ capabilities: ZERODHA, isLoaded: true })
  })

  // Logging out (also on a force_logout from another device) must not leave
  // the old broker's capabilities for whoever signs in next.
  it('is cleared by logout, including a fetch still in flight', async () => {
    useBrokerStore.setState({ capabilities: ZERODHA, isLoaded: true })
    const resolve = deferredFetch()
    const pending = useBrokerStore.getState().fetchCapabilities()

    useAuthStore.getState().logout()
    expect(useBrokerStore.getState()).toMatchObject({ capabilities: null, isLoaded: false })

    resolve(answer(ZERODHA))
    await pending
    expect(useBrokerStore.getState()).toMatchObject({ capabilities: null, isLoaded: false })
  })
})

// STATE-01: a broker session ending or starting (the server's
// active_sessions_update) drops the old session's capabilities, ignores an
// answer still in flight for it, and reads them again for the new one.
describe('broker session change', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it("applies only the new session's capabilities", async () => {
    const FYERS = { ...ZERODHA, broker_name: 'fyers' }
    const answers: Array<(r: Response) => void> = []
    vi.stubGlobal(
      'fetch',
      vi.fn(
        () =>
          new Promise<Response>((resolve) => {
            answers.push(resolve)
          })
      )
    )
    const old = useBrokerStore.getState().fetchCapabilities()
    const fresh = useBrokerStore.getState().sessionChanged(true)
    expect(useBrokerStore.getState().capabilities).toBeNull()
    answers[1](answer(FYERS))
    await fresh
    answers[0](answer(ZERODHA))
    await old
    expect(useBrokerStore.getState().capabilities?.broker_name).toBe('fyers')
  })

  it('drops the capabilities when the session ends', async () => {
    useBrokerStore.setState({ capabilities: ZERODHA, isLoaded: true })
    const fetchSpy = vi.fn()
    vi.stubGlobal('fetch', fetchSpy)
    await useBrokerStore.getState().sessionChanged(false)
    expect(useBrokerStore.getState().capabilities).toBeNull()
    expect(fetchSpy).not.toHaveBeenCalled()
  })
})

// Web: the capabilities request is shared while it is in flight: the navbar
// and its account controls both ask at mount, and the server should be asked
// once.
const capabilities = {
  broker_name: 'fixture',
  broker_type: 'IN_stock',
  supported_exchanges: ['NSE'],
  leverage_config: false,
}

describe('useBrokerStore.fetchCapabilities', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    useBrokerStore.getState().clearCapabilities()
  })

  it('sends one request for callers that ask while it is in flight', async () => {
    const fetchMock = vi.fn(
      async () => new Response(JSON.stringify({ status: 'success', data: capabilities }))
    )
    vi.stubGlobal('fetch', fetchMock)

    const { fetchCapabilities } = useBrokerStore.getState()
    await Promise.all([fetchCapabilities(), fetchCapabilities()])

    expect(fetchMock).toHaveBeenCalledTimes(1)
    expect(useBrokerStore.getState().capabilities).toEqual(capabilities)
  })

  it('asks again once the earlier request has settled', async () => {
    const fetchMock = vi.fn(
      async () => new Response(JSON.stringify({ status: 'success', data: capabilities }))
    )
    vi.stubGlobal('fetch', fetchMock)

    await useBrokerStore.getState().fetchCapabilities()
    await useBrokerStore.getState().fetchCapabilities()

    expect(fetchMock).toHaveBeenCalledTimes(2)
  })
})
