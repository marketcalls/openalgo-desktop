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
}

export const useBrokerStore = create<BrokerStore>()((set, get) => ({
  capabilities: null,
  isLoaded: false,
  epoch: 0,

  fetchCapabilities: async () => {
    const epoch = get().epoch
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
    }
  },

  clearCapabilities: () =>
    set((state) => ({ capabilities: null, isLoaded: false, epoch: state.epoch + 1 })),
}))
