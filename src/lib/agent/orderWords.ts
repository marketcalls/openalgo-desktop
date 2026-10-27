// Desktop-only (D-047): a paused order in a trader's words.
//
// The web's approval card shows the tool's name and its arguments as JSON.
// What the trader approves is an order, so the desktop card states it the
// way a trader would read it back: side, quantity, symbol, exchange,
// product, and how it is priced. The words come only from the call's own
// arguments; the server's risk guard still checks the order after the
// approval and can still refuse it.

/** One paused call, as the confirm frame carries it. */
export interface ApprovalRequest {
  tool_name: string
  args: Record<string, unknown>
  missing?: string[]
}

/** The card's text: one sentence, and the arguments it was built from. */
export interface ApprovalWords {
  sentence: string
  details: Array<{ name: string; label: string; value: string }>
}

const PRODUCTS: Record<string, string> = {
  CNC: 'delivery (CNC)',
  NRML: 'carry forward (NRML)',
  MIS: 'intraday (MIS)',
}

// The same aliases the server reads (agent/tools/order_vocab.rs), so the
// card says what the order will be, not how the model spelled it.
const PRODUCT_ALIASES: Record<string, string> = {
  INTRADAY: 'MIS',
  DELIVERY: 'CNC',
  CARRYFORWARD: 'NRML',
  CARRY_FORWARD: 'NRML',
  NORMAL: 'NRML',
}

const PRICE_TYPE_ALIASES: Record<string, string> = {
  SL_M: 'SL-M',
  SLM: 'SL-M',
  STOPLOSS_MARKET: 'SL-M',
  STOP_LOSS_MARKET: 'SL-M',
  STOPLOSS: 'SL',
  STOP_LOSS: 'SL',
  STOPLOSS_LIMIT: 'SL',
  STOP_LOSS_LIMIT: 'SL',
}

const LABELS: Record<string, string> = {
  order_id: 'Order id',
  symbol: 'Symbol',
  exchange: 'Exchange',
  action: 'Side',
  quantity: 'Quantity',
  position_size: 'Target position',
  product: 'Product',
  price_type: 'Price type',
  price: 'Limit price',
  trigger_price: 'Trigger price',
}

function text(value: unknown): string {
  if (value === null || value === undefined) return ''
  return String(value).trim()
}

function upper(value: unknown): string {
  return text(value).toUpperCase()
}

function number(value: unknown): number | null {
  if (value === null || value === undefined || value === '') return null
  const n = typeof value === 'number' ? value : Number(String(value).trim())
  return Number.isFinite(n) ? n : null
}

/** A price as a trader reads it: Indian grouping, two decimals. */
export function rupees(value: number): string {
  return new Intl.NumberFormat('en-IN', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(value)
}

function side(value: unknown): string {
  const a = upper(value)
  if (a === 'BUY' || a === 'B') return 'Buy'
  if (a === 'SELL' || a === 'S') return 'Sell'
  return a ? `${a} (not a side)` : ''
}

function product(value: unknown): string {
  const p = upper(value)
  const code = PRODUCT_ALIASES[p] ?? p
  return PRODUCTS[code] ?? (code ? `product ${code}` : '')
}

function priceType(value: unknown): string {
  const t = upper(value)
  const folded = t.replace(/[\s_-]+/g, '_')
  return PRICE_TYPE_ALIASES[folded] ?? t
}

/** The symbol and exchange, reading an `NSE:SBIN` spelling as the server does. */
function venue(args: Record<string, unknown>): { symbol: string; exchange: string } {
  let symbol = upper(args.symbol)
  let exchange = upper(args.exchange)
  const colon = symbol.indexOf(':')
  if (colon >= 0) {
    const prefix = symbol.slice(0, colon).trim()
    symbol = symbol.slice(colon + 1).trim()
    if (!exchange) exchange = prefix
  }
  return { symbol, exchange }
}

/** "SBIN on NSE". */
function instrument(args: Record<string, unknown>): string {
  const { symbol, exchange } = venue(args)
  return exchange ? `${symbol} on ${exchange}` : symbol
}

/** "the SBIN position on NSE". */
function position(args: Record<string, unknown>): string {
  const { symbol, exchange } = venue(args)
  return exchange ? `the ${symbol} position on ${exchange}` : `the ${symbol} position`
}

/** How the order is priced, in words. */
function pricing(args: Record<string, unknown>): string {
  const kind = priceType(args.price_type || 'MARKET')
  const price = number(args.price)
  const trigger = number(args.trigger_price)
  switch (kind) {
    case 'MARKET':
      return 'at market'
    case 'LIMIT':
      return price ? `at a limit price of ${rupees(price)}` : 'at a limit price'
    case 'SL':
      return `as a stop-loss limit order, triggered at ${trigger ? rupees(trigger) : 'no trigger'}, limit price ${price ? rupees(price) : 'none'}`
    case 'SL-M':
      return `as a stop-loss market order, triggered at ${trigger ? rupees(trigger) : 'no trigger'}`
    default:
      return `with price type ${kind}`
  }
}

function order(args: Record<string, unknown>): string {
  const parts = [side(args.action), text(args.quantity), instrument(args)].filter(Boolean)
  return `${parts.join(' ')}, ${product(args.product)}, ${pricing(args)}`
}

function target(value: unknown): string {
  const n = number(value)
  if (n === null) return 'the target position'
  if (n > 0) return `long ${n}`
  if (n < 0) return `short ${Math.abs(n)}`
  return 'flat'
}

function sentence(tool: string, args: Record<string, unknown>): string | null {
  switch (tool) {
    case 'place_order':
      return `${order(args)}.`
    case 'place_smart_order':
      return `Bring ${position(args)}, ${product(args.product)}, to ${target(args.position_size)}, ${pricing(args)}.`
    case 'modify_order':
      return `Change order ${text(args.order_id)} to: ${order(args).replace(/^[A-Z]/, (c) => c.toLowerCase())}.`
    case 'cancel_order':
      return `Cancel order ${text(args.order_id)}.`
    case 'cancel_all_orders':
      return 'Cancel every open order on the account, including orders placed by hand and by other strategies.'
    case 'close_position':
      return `Close ${position(args)}, ${product(args.product)}, at market.`
    case 'close_all_positions':
      return 'Close every open position on the account at market, including positions opened by hand and by other strategies.'
    default:
      return null
  }
}

function details(args: Record<string, unknown>): ApprovalWords['details'] {
  return Object.entries(args)
    .filter(([, v]) => v !== null && v !== undefined && v !== '')
    .map(([k, v]) => ({
      name: k,
      label: LABELS[k] ?? k.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase()),
      value: typeof v === 'object' ? JSON.stringify(v) : String(v),
    }))
}

/**
 * The card's words for one paused call. A call missing a required
 * argument is not put into a sentence (it would read as an order the
 * trader did not ask for); its details are listed and the card refuses
 * the approval.
 */
export function describeApproval(request: ApprovalRequest): ApprovalWords {
  const args = request.args ?? {}
  const incomplete = (request.missing ?? []).length > 0
  const words = incomplete ? null : sentence(request.tool_name, args)
  const fallback = request.tool_name.replace(/_/g, ' ').replace(/^./, (c) => c.toUpperCase())
  return { sentence: words ?? `${fallback}.`, details: details(args) }
}
