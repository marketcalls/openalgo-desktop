/**
 * The scalping terminal's predefined stop-loss and target (desktop, UI-03, to
 * be offered upstream).
 *
 * The web measures them from whatever the WebSocket cache holds when the
 * entry is accepted: an old tick sets the stop at the wrong distance, and no
 * tick, or a value that is not a number, skips it without a word while the
 * panel promises it on every entry. Here the values are checked before the
 * order is sent, the price is a live tick no older than the page's freshness
 * window or else a quote read at that moment, and when neither exists the
 * trader is told the position has no stop.
 */

export type PredefinedUnit = 'PTS' | 'PCT'

export interface PredefinedConfig {
  slOn: boolean
  slValue: string
  slUnit: PredefinedUnit
  tgtOn: boolean
  tgtValue: string
  tgtUnit: PredefinedUnit
}

export const PREDEFINED_STOP_NOT_SET =
  'Order placed, but the predefined stop-loss could not be set because no current price was available. Set it from the position row.'

function positive(value: string): number | null {
  const n = Number(value)
  return value.trim() !== '' && Number.isFinite(n) && n > 0 ? n : null
}

/**
 * Why the enabled predefined values cannot protect an entry, or null when
 * they can. An entry is not sent while this returns a reason.
 */
export function predefinedConfigProblem(cfg: PredefinedConfig): string | null {
  if (cfg.slOn) {
    const sl = positive(cfg.slValue)
    if (sl === null) return 'Enter a stop-loss above 0, or untick SL.'
    if (cfg.slUnit === 'PCT' && sl >= 100) return 'Enter a stop-loss below 100%, or untick SL.'
  }
  if (cfg.tgtOn && positive(cfg.tgtValue) === null) {
    return 'Enter a target above 0, or untick Target.'
  }
  return null
}

export type PredefinedLevels =
  | { ok: true; initialSl: number; target: number }
  | { ok: false; reason: string }

/**
 * The stop and target for an entry at `price`, a reason when the stop cannot
 * sit on the loss side of that price, or null when nothing is enabled.
 */
export function predefinedLevels(
  cfg: PredefinedConfig,
  price: number,
  side: 'BUY' | 'SELL'
): PredefinedLevels | null {
  const points = (on: boolean, value: string, unit: PredefinedUnit) => {
    const n = on ? positive(value) : null
    if (n === null) return 0
    return unit === 'PCT' ? (price * n) / 100 : n
  }
  const slPts = points(cfg.slOn, cfg.slValue, cfg.slUnit)
  const tgtPts = points(cfg.tgtOn, cfg.tgtValue, cfg.tgtUnit)
  if (slPts <= 0 && tgtPts <= 0) return null
  const isBuy = side === 'BUY'
  if (isBuy && slPts >= price) {
    return {
      ok: false,
      reason: `Order placed, but the predefined stop-loss of ${slPts} points is more than the price of ${price}, so no stop was set. Set it from the position row.`,
    }
  }
  // No-SL sentinel that never triggers (target only): below for long, far above for short.
  const initialSl =
    slPts > 0 ? (isBuy ? price - slPts : price + slPts) : isBuy ? 0 : Number.MAX_SAFE_INTEGER
  const target = tgtPts > 0 ? (isBuy ? price + tgtPts : price - tgtPts) : 0
  return { ok: true, initialSl, target }
}

/**
 * The live price a predefined stop is measured from: the WebSocket tick when
 * it is younger than `maxAgeMs`, otherwise a quote read now. Null when
 * neither gives a price.
 */
export async function currentEntryPrice(
  tick: { data?: { ltp?: number }; lastUpdate?: number } | undefined,
  nowMs: number,
  maxAgeMs: number,
  readQuote: () => Promise<number | undefined>
): Promise<number | null> {
  const ltp = tick?.data?.ltp
  if (ltp != null && ltp > 0 && tick?.lastUpdate != null && nowMs - tick.lastUpdate < maxAgeMs) {
    return ltp
  }
  try {
    const quoted = await readQuote()
    return quoted != null && quoted > 0 ? quoted : null
  } catch {
    return null
  }
}
