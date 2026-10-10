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
