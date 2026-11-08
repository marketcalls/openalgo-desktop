/**
 * Desktop: a broker whose feed has no market depth (IndMoney, as on the
 * web) refuses a Depth subscription. Pages that ask for Depth only to read
 * the live price (the option chain, Strategy Builder, Arbitrage, the order
 * dialog) would then get no live price at all. MarketDataManager keeps the
 * symbol at Quote instead, hands Depth consumers what Quote carries with an
 * empty book, and says the book is not available, so a page can tell the
 * trader why it is empty.
 *
 * This class only keeps the book: which Depth requests are in flight,
 * which symbols the broker refused Depth for, for which broker. It is
 * cleared when the feed authenticates for another broker, and asks for the
 * Quote fallback once per refused symbol, so a refusal never loops.
 */

export interface FeedSymbol {
  symbol: string
  exchange: string
}

interface SubscribeItem {
  symbol?: unknown
  exchange?: unknown
  status?: unknown
}

const keyOf = (symbol: string, exchange: string) =>
  `${exchange.toUpperCase()}:${symbol.toUpperCase()}`

export class DepthFallback {
  private broker: string | null = null
  private pending = new Map<string, FeedSymbol[]>()
  private unavailable = new Set<string>()
  private listeners = new Set<() => void>()
  private nextRequest = 0

  /** A request id for a Depth subscribe of `symbols`, to match its answer. */
  track(symbols: FeedSymbol[]): string {
    this.nextRequest += 1
    const id = `desktop-depth-${this.nextRequest}`
    this.pending.set(id, symbols)
    // A request the server never answers is forgotten after a while.
    if (this.pending.size > 64) {
      const oldest = this.pending.keys().next().value
      if (oldest !== undefined) this.pending.delete(oldest)
    }
    return id
  }

  /** The broker the feed authenticated for; another one clears the book. */
  setBroker(broker: unknown): void {
    const next = typeof broker === 'string' ? broker : null
    if (next === this.broker) return
    this.broker = next
    this.pending.clear()
    if (this.unavailable.size > 0) {
      this.unavailable.clear()
      this.notify()
    }
  }

  /**
   * The symbols of a Depth request the server refused, newly: from its
   * subscribe answer (an error item) or an error frame naming the request.
   * Each is marked unavailable; one already marked is not returned again.
   */
  refused(message: {
    request_id?: unknown
    subscriptions?: unknown
    code?: unknown
  }): FeedSymbol[] {
    const id = typeof message.request_id === 'string' ? message.request_id : null
    if (!id) return []
    const asked = this.pending.get(id)
    if (!asked) return []
    this.pending.delete(id)
    let refused: FeedSymbol[] = []
    if (Array.isArray(message.subscriptions)) {
      for (const item of message.subscriptions as SubscribeItem[]) {
        if (item.status !== 'error') continue
        const hit = asked.find(
          (s) =>
            typeof item.symbol === 'string' &&
            typeof item.exchange === 'string' &&
            keyOf(s.symbol, s.exchange) === keyOf(item.symbol, item.exchange)
        )
        if (hit) refused.push(hit)
      }
    } else if (message.code === 'INVALID_MODE') {
      refused = asked
    }
    const fresh = refused.filter((s) => !this.unavailable.has(keyOf(s.symbol, s.exchange)))
    for (const s of fresh) this.unavailable.add(keyOf(s.symbol, s.exchange))
    if (fresh.length > 0) this.notify()
    return fresh
  }

  /**
   * What a Depth consumer of a symbol without a book receives: the quote,
   * the best bid and ask from the quote's own fields, an empty book.
   */
  fill(
    symbol: string,
    exchange: string,
    data: { bid_size?: number; ask_size?: number; depth?: { buy: unknown[]; sell: unknown[] } },
    payload: Record<string, unknown>
  ): void {
    const num = (k: string) => (typeof payload[k] === 'number' ? (payload[k] as number) : undefined)
    data.bid_size = data.bid_size ?? num('bid_qty') ?? num('bid_quantity')
    data.ask_size = data.ask_size ?? num('ask_qty') ?? num('ask_quantity')
    if (this.isUnavailable(symbol, exchange) && !payload.depth) {
      data.depth = { buy: [], sell: [] }
    }
  }

  isUnavailable(symbol: string, exchange: string): boolean {
    return this.unavailable.has(keyOf(symbol, exchange))
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  private notify(): void {
    for (const l of this.listeners) l()
  }
}
