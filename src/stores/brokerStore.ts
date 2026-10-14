import { create } from 'zustand'

interface BrokerCapabilities {
  broker_name: string
  broker_type: 'IN_stock' | 'crypto'
  supported_exchanges: string[]
  leverage_config: boolean
}

interface BrokerStore {
  capabilities: BrokerCapabilities | null
  isLoaded: boolean
  /** Bumped by every clear; an answer to a fetch from an older epoch is stale. */
  epoch: number

  fetchCapabilities: () => Promise<void>
  clearCapabilities: () => void
  /**
   * Desktop: the broker session ended or another one started. Capabilities
   * belong to one session, so they are dropped (an answer still in flight
   * for the old session is ignored) and, when a session is live, read
   * again for it.
   */
  sessionChanged: (live: boolean) => Promise<void>
}

// The navbar and its account controls both ask at mount, before either has an
// answer; they share the request in flight instead of sending two.
// Desktop: shared within one epoch only, so a fetch after a clear (a broker
// session change) asks the server again instead of waiting on an answer for
// the session that ended, which the epoch check would then discard.
let inflight: { epoch: number; done: Promise<void> } | null = null

export const useBrokerStore = create<BrokerStore>()((set, get) => ({
  capabilities: null,
  isLoaded: false,
  epoch: 0,

  fetchCapabilities: () => {
    const epoch = get().epoch
    if (inflight?.epoch === epoch) return inflight.done
    const request = { epoch, done: Promise.resolve() }
    request.done = (async () => {
      try {
        const response = await fetch('/api/broker/capabilities', {
          credentials: 'include',
        })

        if (response.ok) {
          const data = await response.json()
          // Cleared (logout, broker change) while this was in flight: the
          // answer describes a session that has ended.
          if (get().epoch !== epoch) return
          if (data.status === 'success' && data.data) {
            set({ capabilities: data.data, isLoaded: true })
          }
        }
      } catch {
        // Silently fail — capabilities will be null, pages fall back to showing all exchanges
      } finally {
        if (inflight === request) inflight = null
      }
    })()
    inflight = request
    return request.done
  },

  clearCapabilities: () =>
    set((state) => ({ capabilities: null, isLoaded: false, epoch: state.epoch + 1 })),

  sessionChanged: async (live) => {
    get().clearCapabilities()
    if (live) await get().fetchCapabilities()
  },
}))
