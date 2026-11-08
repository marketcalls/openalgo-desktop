/**
 * Desktop: the /trading terminal subscribes Depth for every tradeable
 * symbol and takes its live price from the Depth frames. A broker without
 * market depth (IndMoney, as on the web) refuses that subscription, which
 * would leave the chart without live ticks. This keeps, per terminal, the
 * symbols whose Depth the broker refused, so the terminal follows them at
 * Quote instead (its LTP events carry the price), once each, and tells the
 * trader why the book stays empty.
 *
 * The refusal is read from the feed's control frames (`OpenAlgoWsFeed`
 * `onControl`): an error item for the symbol in a subscribe answer, or an
 * INVALID_MODE error frame.
 */

import type { WsControlMessage } from 'openalgo-charts'

export const DEPTH_UNAVAILABLE =
  'Your broker does not stream market depth, so the chart follows the quote and the book stays empty.'

const keyOf = (symbol: string, exchange: string) =>
  `${exchange.toUpperCase()}:${symbol.toUpperCase()}`

/** Whether `m` refuses `symbol`'s Depth subscription. */
export function refusesDepth(m: WsControlMessage, symbol: string, exchange: string): boolean {
  if (m.status === 'error' && m.code === 'INVALID_MODE') return true
  if (m.type !== 'subscribe' || !Array.isArray(m.subscriptions)) return false
  return (m.subscriptions as Array<Record<string, unknown>>).some(
    (item) =>
      item.status === 'error' &&
      typeof item.symbol === 'string' &&
      typeof item.exchange === 'string' &&
      keyOf(item.symbol, item.exchange) === keyOf(symbol, exchange)
  )
}

/** The symbols one terminal's feed may not stream at Depth. */
export class DepthRefusals {
  private refused = new Set<string>()

  /**
   * Whether `m` newly refuses `symbol`'s Depth: true once, the first time,
   * so the terminal switches to Quote once and never loops.
   */
  takeRefusal(m: WsControlMessage, symbol: string, exchange: string): boolean {
    const key = keyOf(symbol, exchange)
    if (this.refused.has(key) || !refusesDepth(m, symbol, exchange)) return false
    this.refused.add(key)
    return true
  }

  /** The mode the terminal subscribes `symbol` at: Quote once its Depth was refused. */
  modeFor(symbol: string, exchange: string): 'Depth' | 'Quote' {
    return this.refused.has(keyOf(symbol, exchange)) ? 'Quote' : 'Depth'
  }

  /** A new feed (another broker session): nothing is known to be refused. */
  clear(): void {
    this.refused.clear()
  }
}
