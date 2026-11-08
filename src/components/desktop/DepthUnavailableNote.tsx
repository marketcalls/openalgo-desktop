import { useSyncExternalStore } from 'react'
import { MarketDataManager } from '@/lib/MarketDataManager'

/**
 * Desktop: whether the connected broker does not stream `symbol`'s order
 * book (it refused the Depth subscription; MarketDataManager then keeps
 * the symbol at Quote and hands the book empty).
 */
export function useDepthUnavailable(symbol: string | undefined, exchange: string | undefined) {
  const manager = MarketDataManager.getInstance()
  return useSyncExternalStore(
    (onChange) => manager.onDepthAvailabilityChange(onChange),
    () => !!symbol && !!exchange && manager.isDepthUnavailable(symbol, exchange)
  )
}

/** Desktop: tells the trader why the order book stays empty. */
export function DepthUnavailableNote({ symbol, exchange }: { symbol?: string; exchange?: string }) {
  const unavailable = useDepthUnavailable(symbol, exchange)
  if (!unavailable) return null
  return (
    <p role="status" className="text-xs text-muted-foreground">
      Your broker does not stream market depth, so the order book stays empty. Prices and quotes
      still update.
    </p>
  )
}
