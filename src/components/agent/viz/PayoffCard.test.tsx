/**
 * The payoff card, which the web ships without a test (desktop, Q-1040).
 *
 * The curve itself is `strategyMath`'s and has its own tests; what is pinned
 * here is what the card adds around it: the legs are always listed, a known
 * two-leg structure is named and nothing else is, an unlimited loss is the
 * word rather than a number, a frame with no price draws nothing, and a
 * malformed leg costs that leg rather than the conversation (D-031).
 */

import { render, screen } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { PayoffCard } from './PayoffCard'

vi.mock('@/components/strategy-builder/PayoffChart', () => ({
  PayoffChart: () => <div data-testid="payoff-chart" />,
}))

const EXPIRY_TS = Math.floor(Date.now() / 1000) + 5 * 86_400

function leg(overrides: Record<string, unknown>) {
  return {
    id: 'agent-1',
    segment: 'OPTION',
    side: 'SELL',
    lots: 1,
    lotSize: 75,
    expiry: '08SEP26',
    strike: 23850,
    optionType: 'CE',
    price: 120.5,
    iv: 12.5,
    active: true,
    symbol: 'NIFTY08SEP2623850CE',
    exchange: 'NFO',
    contractValid: true,
    expiryTs: EXPIRY_TS,
    origin: 'named',
    ...overrides,
  }
}

const CALL = leg({})
const PUT = leg({ id: 'agent-2', optionType: 'PE', price: 95.25, symbol: 'NIFTY08SEP2623850PE' })

const STRADDLE = {
  underlying: 'NIFTY',
  underlying_exchange: 'NSE_INDEX',
  spot: 23873.45,
  atm_iv: 12.5,
  currency: '',
  mode: 'live',
  as_of: '2026-09-03T12:44:10+05:30',
  timezone: 'Asia/Kolkata',
  legs: [CALL, PUT],
}

describe('PayoffCard', () => {
  it('lists the legs, names the structure and shows an unlimited loss as a word', () => {
    render(<PayoffCard spec={STRADDLE} />)
    expect(screen.getByText('NIFTY Short straddle')).toBeInTheDocument()
    expect(screen.getByText('NIFTY08SEP2623850CE')).toBeInTheDocument()
    expect(screen.getByText('NIFTY08SEP2623850PE')).toBeInTheDocument()
    expect(screen.getAllByText('1x75')).toHaveLength(2)
    expect(screen.getByText('Net credit')).toBeInTheDocument()
    expect(screen.getByText('Unlimited loss')).toBeInTheDocument()
    expect(screen.getByTestId('payoff-chart')).toBeInTheDocument()
  })

  it('names nothing it cannot name unambiguously', () => {
    render(<PayoffCard spec={{ ...STRADDLE, legs: [CALL] }} title="NIFTY payoff at expiry" />)
    expect(screen.getByText('NIFTY payoff at expiry')).toBeInTheDocument()
    expect(screen.queryByText(/straddle|strangle|spread/i)).toBeNull()
  })

  it('marks a leg that came from the position book', () => {
    render(<PayoffCard spec={{ ...STRADDLE, legs: [leg({ origin: 'position' })] }} />)
    expect(screen.getByText('held')).toBeInTheDocument()
  })

  it('names what it did not model, including a row it cannot read', () => {
    render(
      <PayoffCard
        spec={{
          ...STRADDLE,
          excluded: [
            { symbol: 'RELIANCE', reason: 'a cash position rather than a derivative' },
            null,
            'INFY',
          ],
        }}
      />
    )
    expect(
      screen.getByText('Not modelled: RELIANCE - a cash position rather than a derivative; INFY')
    ).toBeInTheDocument()
  })

  it.each([
    ['null', null],
    ['a string', 'payoff'],
    ['no legs', { ...STRADDLE, legs: [] }],
    ['no spot', { ...STRADDLE, spot: 0 }],
    ['a spot that is not a number', { ...STRADDLE, spot: 'high' }],
  ])('says the payoff could not be drawn for %s', (_name, spec) => {
    render(<PayoffCard spec={spec} />)
    expect(screen.getByText(/The payoff could not be drawn/)).toBeInTheDocument()
  })

  it.each([
    ['a null leg', [null, CALL]],
    ['a number and a string', [7, 'NIFTY08SEP2623850CE', CALL]],
    ['a leg with no side', [leg({ side: undefined }), CALL]],
    ['a leg with no lot size', [leg({ lotSize: null }), CALL]],
    ['legs whose fields are the wrong shape', [leg({ strike: 'x', price: {}, iv: [] }), CALL]],
  ])('keeps the readable legs and says one was left out for %s (D-031)', (_name, legs) => {
    expect(() => render(<PayoffCard spec={{ ...STRADDLE, legs }} />)).not.toThrow()
    expect(screen.getAllByText('NIFTY08SEP2623850CE').length).toBeGreaterThan(0)
  })

  it('says how many legs it could not read (D-031)', () => {
    render(<PayoffCard spec={{ ...STRADDLE, legs: [null, 3, CALL] }} />)
    expect(
      screen.getByText('2 legs in this frame could not be read, so the curve leaves them out.')
    ).toBeInTheDocument()
  })
})
