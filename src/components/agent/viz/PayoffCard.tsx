/**
 * A payoff diagram for an option structure, in the conversation.
 *
 * **The maths is not here, and must never be.** `strategyMath.ts` computes the
 * curve and `PayoffChart` draws it; both are what `/strategybuilder` uses. A
 * second implementation would drift from that one, and the copy living in the
 * chat is the copy nobody notices is wrong. This file resolves nothing and
 * calculates no payoff: it hands served legs to `computePayoff` and the result
 * to the existing chart. That is the same reasoning `CLAUDE.md` gives for
 * `services/risk/`.
 *
 * **Every number arrives priced.** The backend fetched each leg's strike, lot
 * size, expiry, premium and IV from a service and emits them in exactly the
 * `StrategyLeg` shape, so nothing here is inferred and the model never supplied
 * a price. `spot` is the underlying's real last trade; without it the backend
 * refuses to emit at all rather than centring a curve on nothing.
 *
 * **Unlimited is a real value.** A naked short call's maximum loss is genuinely
 * `-Infinity`, and `computePayoff` returns exactly that. Rendering it as a
 * number would be a lie about the risk, and rendering it blank reads as missing
 * data, so it is rendered as the word.
 */

import { useMemo } from 'react'
import { PayoffChart } from '@/components/strategy-builder/PayoffChart'
import { Alert, AlertDescription } from '@/components/ui/alert'
import {
  computePayoff,
  type PayoffResult,
  type ScenarioState,
  type StrategyLeg,
} from '@/lib/strategyMath'
import { cn } from '@/lib/utils'
import { asNumber, asRecord, asText } from './spec'

/** Sample count for the curve. The chart is inline and narrow; 240 is what
 *  /strategybuilder uses and it is already more points than pixels here. */
const CURVE_STEPS = 240

/** How far either side of spot the curve is drawn, as a fraction of spot.
 *  Wide enough that both breakevens of an ATM straddle sit inside the frame. */
const RANGE_FRACTION = 0.12

const MS_PER_DAY = 86_400_000

export interface PayoffLeg extends StrategyLeg {
  /** `named` when the operator listed it, `position` when it came from the book. */
  origin?: string
}

export interface PayoffSpec {
  underlying?: string
  underlying_exchange?: string
  spot?: number
  atm_iv?: number | null
  currency?: string
  mode?: string
  as_of?: string
  timezone?: string
  legs?: PayoffLeg[]
  /** Legs the tool refused to model, with the reason. Cash equity is the common one. */
  excluded?: Array<{ symbol?: string; reason?: string }> | string[]
  notices?: string[]
}

/**
 * Read one leg off the frame, or `null` when it cannot be modelled.
 *
 * Desktop (D-031): the web cast `spec.legs` straight to `PayoffLeg[]`, so a
 * `null` or a number in the list threw while the card rendered, outside the
 * guard around `computePayoff`, and took the whole message down with it. Each
 * leg is now read the way `spec.ts` reads every other frame: a leg needs a
 * symbol, a side and a positive size, because a guessed side flips the curve
 * and a guessed size scales it. Everything else falls back to what the
 * strategy builder uses for "unknown".
 */
function parseLeg(value: unknown, index: number): PayoffLeg | null {
  const raw = asRecord(value)
  if (!raw) return null
  const symbol = asText(raw.symbol)
  const side = asText(raw.side)?.toUpperCase()
  const lots = asNumber(raw.lots)
  const lotSize = asNumber(raw.lotSize)
  if (!symbol || (side !== 'BUY' && side !== 'SELL')) return null
  if (lots === null || lots <= 0 || lotSize === null || lotSize <= 0) return null
  const optionType = asText(raw.optionType)?.toUpperCase()
  const leg: PayoffLeg = {
    id: asText(raw.id) ?? `leg-${index + 1}`,
    segment: asText(raw.segment)?.toUpperCase() === 'FUTURE' ? 'FUTURE' : 'OPTION',
    side,
    lots,
    lotSize,
    expiry: asText(raw.expiry) ?? '',
    price: asNumber(raw.price) ?? 0,
    iv: asNumber(raw.iv) ?? 0,
    active: raw.active !== false,
    symbol,
  }
  const strike = asNumber(raw.strike)
  if (strike !== null) leg.strike = strike
  if (optionType === 'CE' || optionType === 'PE') leg.optionType = optionType
  const exchange = asText(raw.exchange)
  if (exchange) leg.exchange = exchange
  for (const key of [
    'expiryTs',
    'tickSize',
    'marketPrice',
    'referenceUnderlying',
    'forwardPrice',
  ] as const) {
    const number = asNumber(raw[key])
    if (number !== null) leg[key] = number
  }
  if (raw.contractValid === true) leg.contractValid = true
  const origin = asText(raw.origin)
  if (origin) leg.origin = origin
  return leg
}

/** Every leg the frame carried that can be modelled, and how many could not. */
function parseLegs(value: unknown): { legs: PayoffLeg[]; unreadable: number } {
  if (!Array.isArray(value)) return { legs: [], unreadable: 0 }
  const legs = value
    .map((item, index) => parseLeg(item, index))
    .filter((leg): leg is PayoffLeg => leg !== null)
  return { legs, unreadable: value.length - legs.length }
}

/** One excluded row as the card names it, or `null` when it says nothing. */
function excludedText(item: unknown): string | null {
  if (typeof item === 'string') return asText(item)
  const row = asRecord(item)
  if (!row) return null
  return [asText(row.symbol), asText(row.reason)].filter(Boolean).join(' - ') || null
}

/** Calendar days from now to the nearest leg expiry, floored at zero.
 *
 * `computePayoff` wants days to the NEAREST expiry for the terminal curve; the
 * remaining legs are still priced at their own remaining time inside it, so a
 * calendar spread does not need special handling here.
 */
function daysToNearestExpiry(legs: PayoffLeg[], now: Date): number {
  const stamps = legs
    .map((leg) => leg.expiryTs)
    .filter((value): value is number => typeof value === 'number' && value > 0)
  if (stamps.length === 0) return 0
  const soonest = Math.min(...stamps) * 1000
  return Math.max(0, (soonest - now.getTime()) / MS_PER_DAY)
}

function isOptionLike(leg: PayoffLeg): boolean {
  return typeof leg.strike === 'number' && leg.strike > 0
}

/** The premium the structure paid or received, per lot-adjusted leg.
 *
 * Positive means a net credit. Signed by side, so a short straddle reads as
 * money in and a long one as money out.
 */
function netPremium(legs: PayoffLeg[]): number {
  return legs.reduce((total, leg) => {
    const sign = leg.side === 'SELL' ? 1 : -1
    const size = (leg.lots || 0) * (leg.lotSize || 0)
    return total + sign * (leg.price || 0) * size
  }, 0)
}

/** Name the structure from its legs, or return null rather than guess.
 *
 * Only shapes that are unambiguous from strikes and sides are named. A wrong
 * name on a real position is worse than no name, so anything else falls through
 * to the leg list, which is always shown regardless.
 */
function nameStructure(legs: PayoffLeg[]): string | null {
  const options = legs.filter(isOptionLike)
  if (options.length !== legs.length || options.length !== 2) return null

  const [a, b] = options
  const sameSide = a.side === b.side
  const sameStrike = a.strike === b.strike
  const types = [a.optionType, b.optionType].sort().join('')
  const straddleTypes = types === 'CEPE'

  if (sameSide && sameStrike && straddleTypes) {
    return a.side === 'SELL' ? 'Short straddle' : 'Long straddle'
  }
  if (sameSide && !sameStrike && straddleTypes) {
    return a.side === 'SELL' ? 'Short strangle' : 'Long strangle'
  }
  if (!sameSide && a.optionType === b.optionType && !sameStrike) {
    const long = a.side === 'BUY' ? a : b
    const short = a.side === 'BUY' ? b : a
    if (a.optionType === 'CE') {
      return (long.strike ?? 0) < (short.strike ?? 0) ? 'Bull call spread' : 'Bear call spread'
    }
    return (long.strike ?? 0) > (short.strike ?? 0) ? 'Bear put spread' : 'Bull put spread'
  }
  return null
}

function formatUnlimited(value: number, currency: string): string {
  if (!Number.isFinite(value)) return value > 0 ? 'Unlimited' : 'Unlimited loss'
  return `${currency}${Math.abs(value).toLocaleString('en-IN', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: 'good' | 'bad' }) {
  return (
    <div className="min-w-0">
      <div className="text-[10px] leading-none tracking-wide text-muted-foreground uppercase">
        {label}
      </div>
      <div
        className={cn(
          'mt-1 truncate font-mono text-sm tabular-nums',
          tone === 'good' && 'text-emerald-600 dark:text-emerald-400',
          tone === 'bad' && 'text-red-600 dark:text-red-400'
        )}
      >
        {value}
      </div>
    </div>
  )
}

export interface PayoffCardProps {
  spec: unknown
  title?: string
}

export function PayoffCard({ spec, title }: PayoffCardProps) {
  const parsed = asRecord(spec) as PayoffSpec | null
  // One parse per spec object: the memo below keys on `legs`, and a fresh
  // array on every render would recompute the curve each time.
  const { legs, unreadable } = useMemo(() => parseLegs(parsed?.legs), [parsed?.legs])
  const spot = asNumber(parsed?.spot) ?? 0
  const currency = asText(parsed?.currency) ?? ''
  const atmIv = asNumber(parsed?.atm_iv)

  // One memo for the whole computation: computePayoff walks every leg across
  // 240 samples twice, and re-running it on an unrelated re-render is the kind
  // of cost that only shows up once several cards sit in one thread.
  const model = useMemo(() => {
    if (legs.length === 0 || spot <= 0) return null
    const now = new Date()
    const daysAtExpiry = daysToNearestExpiry(legs, now)
    const range: [number, number] = [spot * (1 - RANGE_FRACTION), spot * (1 + RANGE_FRACTION)]
    const fallbackIv = atmIv ?? 0

    let payoff: PayoffResult
    try {
      payoff = computePayoff(legs, spot, daysAtExpiry, 0, range, CURVE_STEPS, 0, fallbackIv, now)
    } catch {
      // A leg the maths cannot price should cost the card its curve, not the
      // whole conversation. The legs and the numbers below still render.
      return null
    }

    const scenario: ScenarioState = {
      spot,
      iv: fallbackIv,
      daysElapsed: 0,
      valuationTime: now,
    }
    return {
      payoff,
      scenario,
      remainingYears: daysAtExpiry / 365,
      credit: netPremium(legs),
    }
  }, [legs, spot, atmIv])

  if (legs.length === 0 || spot <= 0) {
    return (
      <Alert className="my-3">
        <AlertDescription>
          The payoff could not be drawn: the frame carried no priced legs or no underlying price.
        </AlertDescription>
      </Alert>
    )
  }

  const structure = nameStructure(legs)
  const underlying = asText(parsed?.underlying)
  const mode = asText(parsed?.mode)
  const heading = title || [underlying, structure].filter(Boolean).join(' ') || 'Payoff'
  const excluded = (Array.isArray(parsed?.excluded) ? parsed.excluded : [])
    .map(excludedText)
    .filter((text): text is string => text !== null)
  const notices = (Array.isArray(parsed?.notices) ? parsed.notices : [])
    .map(asText)
    .filter((text): text is string => text !== null)

  return (
    <div className="my-3 overflow-hidden rounded-lg border border-border bg-card">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-border bg-muted/40 px-3 py-2">
        <span className="text-sm font-semibold tracking-tight">{heading}</span>
        {underlying && (
          <span className="font-mono text-[11px] text-muted-foreground">
            {underlying} {currency}
            {spot.toLocaleString('en-IN')}
          </span>
        )}
        {mode && (
          <span className="ml-auto text-[10px] tracking-wide text-muted-foreground uppercase">
            {mode}
          </span>
        )}
      </div>

      {/* The legs, always shown. When nameStructure returns null this is the
          only description of what was modelled, so it is never optional. */}
      <ul className="divide-y divide-border/60">
        {legs.map((leg) => (
          <li
            key={leg.id || leg.symbol}
            className="flex flex-wrap items-baseline gap-x-2 px-3 py-1.5 text-[12px]"
          >
            <span
              className={cn(
                'font-medium',
                leg.side === 'SELL'
                  ? 'text-red-600 dark:text-red-400'
                  : 'text-emerald-600 dark:text-emerald-400'
              )}
            >
              {leg.side}
            </span>
            <span className="font-mono text-muted-foreground">
              {leg.lots}x{leg.lotSize}
            </span>
            <span className="font-mono">{leg.symbol}</span>
            <span className="ml-auto font-mono tabular-nums">
              {currency}
              {(leg.price ?? 0).toLocaleString('en-IN', { minimumFractionDigits: 2 })}
            </span>
            {leg.origin === 'position' && (
              <span className="text-[10px] tracking-wide text-muted-foreground uppercase">
                held
              </span>
            )}
          </li>
        ))}
      </ul>

      {model ? (
        <>
          <div className="grid grid-cols-2 gap-x-4 gap-y-3 border-t border-border px-3 py-2.5 sm:grid-cols-4">
            <Stat
              label={model.credit >= 0 ? 'Net credit' : 'Net debit'}
              value={formatUnlimited(model.credit, currency)}
              tone={model.credit >= 0 ? 'good' : undefined}
            />
            <Stat
              label="Breakevens"
              value={
                model.payoff.breakevens.length
                  ? model.payoff.breakevens
                      .map((value) => value.toLocaleString('en-IN', { maximumFractionDigits: 2 }))
                      .join('  ')
                  : 'None'
              }
            />
            <Stat
              label="Max profit"
              value={formatUnlimited(model.payoff.maxProfit, currency)}
              tone="good"
            />
            <Stat
              label="Max loss"
              value={formatUnlimited(model.payoff.maxLoss, currency)}
              tone="bad"
            />
          </div>

          {/* showTplus0 draws the current-value curve alongside the terminal
              one, which is what an operator asking for T+0 wants to see. */}
          <div className="px-1 pb-2">
            <PayoffChart
              title=""
              chartIdentity={legs.map((leg) => `${leg.symbol}:${leg.side}:${leg.lots}`).join('|')}
              scenario={model.scenario}
              remainingYears={model.remainingYears}
              payoff={model.payoff}
              showTplus0
              height={300}
              formatCurrency={(value) =>
                `${currency}${value.toLocaleString('en-IN', { maximumFractionDigits: 0 })}`
              }
            />
          </div>
        </>
      ) : (
        <Alert className="m-3">
          <AlertDescription>
            The legs are listed above, but the curve could not be computed for this structure.
          </AlertDescription>
        </Alert>
      )}

      {/* Excluded legs are surfaced deliberately. Dropping shares held against
          a short call quietly would draw a naked short call where a covered
          call is held, which understates the risk. */}
      {excluded.length > 0 && (
        <div className="border-t border-border px-3 py-2 text-[11px] text-muted-foreground">
          Not modelled: {excluded.join('; ')}
        </div>
      )}

      {unreadable > 0 && (
        <div className="border-t border-border px-3 py-2 text-[11px] text-muted-foreground">
          {unreadable === 1
            ? 'One leg in this frame could not be read, so the curve leaves it out.'
            : `${unreadable} legs in this frame could not be read, so the curve leaves them out.`}
        </div>
      )}

      {notices.length > 0 && (
        <div className="border-t border-border px-3 py-2 text-[11px] text-muted-foreground">
          {notices.join(' ')}
        </div>
      )}
    </div>
  )
}
