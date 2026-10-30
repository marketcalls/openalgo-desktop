/**
 * The Multi-Leg node's manual leg builder, pinned as the web renders it
 * (parity F-651, F-652, F-653, F-654, F-655, F-656, F-658): the template
 * picker and counter, the empty state, Add Leg and its cap, the basket
 * problem and footer, the row header (expand, summary, duplicate, remove,
 * collapsed problem), the side/option-type toggles, the strike and expiry
 * text modes, the price fields per price type, the per-field error ids, and
 * the contract hint's three states. The carried web tests in
 * CustomLegsFields.test.tsx cover the expiry and strike pickers themselves.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { OptionStrikesResponse } from '@/api/flow'
import type { CustomLeg } from '@/lib/flow/customLegs'

const getOptionStrikes = vi.fn<() => Promise<OptionStrikesResponse>>()

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return { ...actual, getOptionStrikes: () => getOptionStrikes() }
})

/** What the component hands the library: the seed request and the raw legs it commits. */
const lib = vi.hoisted(() => ({
  seeds: [] as Array<[string, unknown]>,
  commits: [] as CustomLeg[][],
}))

vi.mock('@/lib/flow/customLegs', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/lib/flow/customLegs')>()
  return {
    ...actual,
    seedLegsFromStrategy: (
      strategy: string,
      options: Parameters<typeof actual.seedLegsFromStrategy>[1]
    ) => {
      lib.seeds.push([strategy, options])
      return actual.seedLegsFromStrategy(strategy, options)
    },
    serializeCustomLegs: (legs: CustomLeg[]) => {
      lib.commits.push(legs)
      return actual.serializeCustomLegs(legs)
    },
  }
})

const { CustomLegsFields } = await import('./CustomLegsFields')

const LISTING: OptionStrikesResponse = {
  underlying: 'NIFTY',
  exchange: 'NFO',
  expiry: '25AUG26',
  expiries: ['25AUG26', '01SEP26', '29SEP26'],
  resolved: {
    current_week: '25AUG26',
    next_week: '01SEP26',
    current_month: '25AUG26',
    next_month: '29SEP26',
  },
  optionType: 'CE',
  strikes: [
    { strike: 24150, symbol: 'NIFTY25AUG2624150CE', label: 'ITM1' },
    { strike: 24200, symbol: 'NIFTY25AUG2624200CE', label: 'ATM' },
    { strike: 24250, symbol: 'NIFTY25AUG2624250CE', label: 'OTM1' },
  ],
  atm: 24200,
  underlyingLtp: 24219.05,
  underlyingSymbol: 'NIFTY',
}

const ATM_CE = { strikeMode: 'OFFSET', offset: 'ATM', optionType: 'CE', action: 'BUY', quantity: 1 }

interface Common {
  commonPriceType?: string
  commonProduct?: string
  commonAction?: string
  commonQuantity?: number
  strangleWidth?: string
}

/** The last legs array the builder wrote back to the node. */
let saved: Array<Record<string, unknown>> = []

function Harness({ initial, common }: { initial: unknown[]; common: Common }) {
  const [legs, setLegs] = useState<unknown[]>(initial)
  return (
    <CustomLegsFields
      value={legs}
      onChange={(next) => {
        saved = next
        setLegs(next)
      }}
      commonPriceType={common.commonPriceType ?? 'MARKET'}
      commonProduct={common.commonProduct ?? 'MIS'}
      commonExpiryType="current_week"
      commonAction={common.commonAction ?? 'BUY'}
      commonQuantity={common.commonQuantity ?? 1}
      strangleWidth={common.strangleWidth ?? 'OTM2'}
      underlying="NIFTY"
    />
  )
}

function mount(initial: unknown[] = [ATM_CE], common: Common = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  const user = userEvent.setup()
  const view = render(
    <QueryClientProvider client={client}>
      <Harness initial={initial} common={common} />
    </QueryClientProvider>
  )
  return { user, ...view }
}

const listed = () => waitFor(() => expect(screen.getByText(/ATM 24200/)).toBeTruthy())

/** The row header toggles, in row order. */
const toggles = () => screen.getAllByRole('button').filter((b) => b.hasAttribute('aria-expanded'))
const openRows = () => toggles().map((b) => b.getAttribute('aria-expanded'))

/** A row's outer box. */
const rowBox = (index: number) => toggles()[index].parentElement?.parentElement as HTMLElement

async function choose(user: ReturnType<typeof userEvent.setup>, label: string, option: string) {
  await user.click(screen.getByLabelText(label))
  await user.click(await screen.findByRole('option', { name: option }))
}

async function optionsOf(user: ReturnType<typeof userEvent.setup>, label: string) {
  await user.click(screen.getByLabelText(label))
  return (await screen.findAllByRole('option')).map((o) => o.textContent)
}

beforeEach(() => {
  saved = []
  lib.seeds = []
  lib.commits = []
  getOptionStrikes.mockReset()
  getOptionStrikes.mockResolvedValue(LISTING)
})

describe('F-651 the header and the readymade strategy picker', () => {
  it('heads the list "Legs" with a {n}/10 counter', () => {
    mount([ATM_CE, ATM_CE, ATM_CE])
    expect(screen.getByText('Legs')).toBeInTheDocument()
    expect(screen.getByText('3/10')).toBeInTheDocument()
  })

  it('offers the five strategies under its own name, showing the placeholder', async () => {
    const { user } = mount()
    const picker = screen.getByLabelText('Load legs from a readymade strategy')
    expect(picker.textContent).toBe('Start from a readymade strategy')
    expect(await optionsOf(user, 'Load legs from a readymade strategy')).toEqual([
      'Straddle',
      'Strangle',
      'Iron Condor',
      'Bull Call Spread',
      'Bear Put Spread',
    ])
  })

  it('says what loading a strategy does', () => {
    mount()
    expect(
      screen.getByText(
        "Loads that strategy's legs here so you can change strikes, expiries and sides. Replaces the legs below."
      )
    ).toBeInTheDocument()
  })

  it('replaces every leg without asking, collapses them, and goes back to its placeholder', async () => {
    const { user } = mount([ATM_CE], { commonQuantity: 2 })
    await choose(user, 'Load legs from a readymade strategy', 'Iron Condor')

    expect(saved).toEqual([
      { strikeMode: 'OFFSET', optionType: 'CE', action: 'SELL', quantity: 2, offset: 'OTM2' },
      { strikeMode: 'OFFSET', optionType: 'CE', action: 'BUY', quantity: 2, offset: 'OTM4' },
      { strikeMode: 'OFFSET', optionType: 'PE', action: 'SELL', quantity: 2, offset: 'OTM2' },
      { strikeMode: 'OFFSET', optionType: 'PE', action: 'BUY', quantity: 2, offset: 'OTM4' },
    ])
    expect(screen.getByText('4/10')).toBeInTheDocument()
    expect(openRows()).toEqual(['false', 'false', 'false', 'false'])
    expect(screen.getByLabelText('Load legs from a readymade strategy').textContent).toBe(
      'Start from a readymade strategy'
    )
  })

  it("seeds with the node's side, quantity and strangle width", async () => {
    const { user } = mount([ATM_CE], {
      commonAction: 'SELL',
      commonQuantity: 3,
      strangleWidth: 'OTM3',
    })
    await choose(user, 'Load legs from a readymade strategy', 'Strangle')

    expect(lib.seeds).toEqual([
      ['strangle', { action: 'SELL', quantity: '3', strangleWidth: 'OTM3' }],
    ])
    expect(saved.map((leg) => [leg.action, leg.quantity, leg.offset])).toEqual([
      ['SELL', 3, 'OTM3'],
      ['SELL', 3, 'OTM3'],
    ])
  })

  it('reads any side but SELL as BUY, and no quantity as one lot', async () => {
    const { user } = mount([ATM_CE], { commonAction: '', commonQuantity: 0 })
    await choose(user, 'Load legs from a readymade strategy', 'Straddle')

    expect(lib.seeds[0]).toEqual([
      'straddle',
      { action: 'BUY', quantity: '1', strangleWidth: 'OTM2' },
    ])
  })
})

describe('F-652 the empty state, Add Leg and the basket problem', () => {
  it('shows the empty state, 0/10 and the basket problem with no legs', () => {
    mount([])
    expect(
      screen.getByText('No legs yet. Add one, or load a readymade strategy above and edit it.')
    ).toBeInTheDocument()
    expect(screen.getByText('0/10')).toBeInTheDocument()
    expect(screen.getByText('Add at least one leg')).toHaveClass('text-destructive')
  })

  it('appends a default leg at the node quantity and opens only that row', async () => {
    const { user } = mount([ATM_CE], { commonQuantity: 4 })
    expect(openRows()).toEqual(['true'])

    await user.click(screen.getByRole('button', { name: 'Add Leg' }))

    expect(saved).toHaveLength(2)
    expect(saved[1]).toEqual({
      strikeMode: 'OFFSET',
      optionType: 'CE',
      action: 'BUY',
      quantity: 4,
      offset: 'ATM',
    })
    expect(openRows()).toEqual(['false', 'true'])
  })

  it('adds a one-lot leg when the node has no quantity', async () => {
    const { user } = mount([], { commonQuantity: 0 })
    await user.click(screen.getByRole('button', { name: 'Add Leg' }))
    expect(saved[0].quantity).toBe(1)
  })

  it('stops adding at ten legs', () => {
    mount(Array.from({ length: 10 }, () => ATM_CE))
    expect(screen.getByText('10/10')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Add Leg' })).toBeDisabled()
  })

  it('still adds at nine', () => {
    mount(Array.from({ length: 9 }, () => ATM_CE))
    expect(screen.getByRole('button', { name: 'Add Leg' })).toBeEnabled()
  })

  it('names the first incomplete leg under the list', () => {
    mount([ATM_CE, { ...ATM_CE, offset: 'XYZ' }])
    expect(screen.getByText('Leg 2 is incomplete')).toHaveClass('text-destructive')
  })

  it('ends with the note about the underlying and calendar spreads', () => {
    mount()
    expect(
      screen.getByText(
        "Every leg is placed against the node's underlying. A leg that names its own expiry is how a calendar or diagonal spread is built; leave it on Same as node otherwise."
      )
    ).toBeInTheDocument()
  })
})

describe('F-653 the leg row header', () => {
  it('opens the first row only and numbers the rows from 1', () => {
    mount([ATM_CE, ATM_CE, ATM_CE])
    expect(openRows()).toEqual(['true', 'false', 'false'])
    expect(toggles().map((t) => t.querySelector('span')?.textContent)).toEqual(['1', '2', '3'])
  })

  it('opens and closes a row from its header', async () => {
    const { user } = mount([ATM_CE, ATM_CE, ATM_CE])
    await user.click(toggles()[1])
    expect(openRows()).toEqual(['true', 'true', 'false'])
    await user.click(toggles()[0])
    expect(openRows()).toEqual(['false', 'true', 'false'])
  })

  it('summarises a bought leg in green and a sold leg in red', () => {
    mount([
      ATM_CE,
      {
        ...ATM_CE,
        action: 'SELL',
        quantity: 2,
        offset: 'OTM3',
        optionType: 'PE',
        expiry: '25AUG26',
      },
    ])
    expect(screen.getByText('BUY 1x ATM CE')).toHaveClass('text-green-600')
    expect(screen.getByText('SELL 2x OTM3 25AUG26 PE')).toHaveClass('text-red-600')
  })

  it('duplicates a leg right after itself and opens the copy', async () => {
    const sold = { ...ATM_CE, action: 'SELL', offset: 'OTM2' }
    const { user } = mount([ATM_CE, sold])
    await user.click(screen.getByRole('button', { name: 'Duplicate leg 1' }))

    expect(saved.map((leg) => [leg.action, leg.offset])).toEqual([
      ['BUY', 'ATM'],
      ['BUY', 'ATM'],
      ['SELL', 'OTM2'],
    ])
    expect(openRows()).toEqual(['false', 'true', 'false'])
  })

  it('removes a leg without asking and closes every row', async () => {
    const { user } = mount([ATM_CE, { ...ATM_CE, offset: 'OTM1' }, { ...ATM_CE, offset: 'OTM2' }])
    await user.click(screen.getByRole('button', { name: 'Remove leg 2' }))

    expect(saved.map((leg) => leg.offset)).toEqual(['ATM', 'OTM2'])
    expect(openRows()).toEqual(['false', 'false'])
  })

  it('shows a closed row its first problem and outlines it in the error colour', () => {
    mount([ATM_CE, { ...ATM_CE, offset: 'XYZ', quantity: 'abc' }])
    expect(screen.getByText('Use ATM, ITM1-ITM50 or OTM1-OTM50')).toBeInTheDocument()
    expect(screen.queryByText('Quantity must be a number')).toBeNull()
    expect(rowBox(1)).toHaveClass('border-destructive/50')
    expect(rowBox(0)).toHaveClass('border-border')
    expect(rowBox(0)).not.toHaveClass('border-destructive/50')
  })
})

describe('F-654 side, option type and strike', () => {
  it('toggles the side with pressed buttons under a hidden legend', async () => {
    const { user } = mount()
    const legend = screen.getByText('Side for leg 1')
    expect(legend.tagName).toBe('LEGEND')
    expect(legend).toHaveClass('sr-only')
    const side = within(legend.parentElement as HTMLElement)
    expect(side.getByRole('button', { name: 'BUY' })).toHaveAttribute('aria-pressed', 'true')
    expect(side.getByRole('button', { name: 'SELL' })).toHaveAttribute('aria-pressed', 'false')

    await user.click(side.getByRole('button', { name: 'SELL' }))
    expect(saved[0].action).toBe('SELL')
  })

  it('toggles the option type with pressed buttons under a hidden legend', async () => {
    const { user } = mount()
    const legend = screen.getByText('Option type for leg 1')
    expect(legend.tagName).toBe('LEGEND')
    const type = within(legend.parentElement as HTMLElement)
    expect(type.getByRole('button', { name: 'CE' })).toHaveAttribute('aria-pressed', 'true')
    expect(type.getByRole('button', { name: 'PE' })).toHaveAttribute('aria-pressed', 'false')

    await user.click(type.getByRole('button', { name: 'PE' }))
    expect(saved[0].optionType).toBe('PE')
  })

  it('offers Offset and Strike, each with its hint', async () => {
    const { user } = mount()
    await listed()
    expect(screen.getByText('Re-resolved against the underlying on every run')).toBeInTheDocument()
    expect(await optionsOf(user, 'Strike mode')).toEqual(['Offset', 'Strike'])
    await user.click(await screen.findByRole('option', { name: 'Strike' }))
    expect(
      await screen.findByText('One named contract, used exactly as entered')
    ).toBeInTheDocument()
  })

  it('writes the offset picked in Offset mode', async () => {
    const { user } = mount()
    await listed()
    await choose(user, 'Strike offset', 'OTM3')
    expect(saved[0].offset).toBe('OTM3')
  })

  it('colours the ATM row of the listed strikes amber', async () => {
    const { user } = mount([{ ...ATM_CE, strikeMode: 'STRIKE', strike: 24200 }])
    await listed()
    await user.click(screen.getByLabelText('Strike price'))
    const listbox = await screen.findByRole('listbox')
    expect(within(listbox).getByText('ATM')).toHaveClass('text-amber-500')
    expect(within(listbox).getByText('ITM1')).toHaveClass('text-muted-foreground')
  })

  it('switches between the listed strikes and a typed strike', async () => {
    const { user } = mount([{ ...ATM_CE, strikeMode: 'STRIKE', strike: 24200 }])
    await listed()
    expect(screen.getByLabelText('Strike price').getAttribute('role')).toBe('combobox')

    await user.click(screen.getByRole('button', { name: 'Type a strike or {{variable}}' }))
    const typed = screen.getByLabelText('Strike price')
    expect(typed.tagName).toBe('INPUT')
    expect(typed).toHaveAttribute('placeholder', '24500')
    fireEvent.change(typed, { target: { value: '24300' } })
    expect(saved[0].strike).toBe(24300)

    await user.click(screen.getByRole('button', { name: 'Pick a listed strike' }))
    expect(screen.getByLabelText('Strike price').getAttribute('role')).toBe('combobox')
  })

  it('keeps a {{variable}} strike as typed text even with a listing', async () => {
    mount([{ ...ATM_CE, strikeMode: 'STRIKE', strike: '{{webhook.strike}}' }])
    await listed()
    const typed = screen.getByLabelText('Strike price') as HTMLInputElement
    expect(typed.tagName).toBe('INPUT')
    expect(typed.value).toBe('{{webhook.strike}}')
    expect(screen.getByRole('button', { name: 'Pick a listed strike' })).toBeInTheDocument()
  })

  it('types the strike, with no toggle, when the contracts cannot be listed', async () => {
    getOptionStrikes.mockRejectedValue(new Error('no broker session'))
    mount([{ ...ATM_CE, strikeMode: 'STRIKE', strike: 24200 }])
    await screen.findByText(/Listed contracts unavailable/)
    expect(screen.getByLabelText('Strike price').tagName).toBe('INPUT')
    expect(screen.queryByRole('button', { name: 'Type a strike or {{variable}}' })).toBeNull()
    expect(screen.queryByRole('button', { name: 'Pick a listed strike' })).toBeNull()
  })
})

describe('F-655 typing an expiry', () => {
  it('switches to a date box that writes an upper-cased DATE expiry, and back', async () => {
    const { user } = mount()
    await listed()
    await user.click(screen.getByRole('button', { name: 'Type a date or {{variable}}' }))

    const typed = screen.getByLabelText('Expiry date')
    expect(typed).toHaveAttribute('placeholder', '28OCT25')
    fireEvent.change(typed, { target: { value: '28oct25' } })
    expect(lib.commits.at(-1)?.[0]).toMatchObject({ expiryMode: 'DATE', expiry: '28OCT25' })
    expect(saved[0].expiry).toBe('28OCT25')

    await user.click(screen.getByRole('button', { name: 'Pick a listed expiry' }))
    expect(screen.getByLabelText('Expiry').getAttribute('role')).toBe('combobox')
  })

  it('writes a picked expiry as a DATE', async () => {
    const { user } = mount()
    await listed()
    await choose(user, 'Expiry', '01SEP26')
    expect(lib.commits.at(-1)?.[0]).toMatchObject({ expiryMode: 'DATE', expiry: '01SEP26' })
  })

  it('says an inherited expiry rolls with the node', async () => {
    mount()
    await listed()
    expect(
      screen.getByText('Follows the node, so it rolls to the next expiry.')
    ).toBeInTheDocument()
  })
})

describe('F-656 quantity, product, price fields and their errors', () => {
  it('writes the quantity in lots', () => {
    mount()
    const quantity = screen.getByLabelText('Quantity (Lots)')
    expect(quantity.id).toBe('custom-leg-0-quantity')
    fireEvent.change(quantity, { target: { value: '3' } })
    expect(saved[0].quantity).toBe(3)
  })

  it('hides CNC unless the leg already trades it', async () => {
    const first = mount()
    expect(await optionsOf(first.user, 'Product')).toEqual(['MIS', 'NRML'])
    first.unmount()

    const cnc = mount([{ ...ATM_CE, product: 'CNC' }])
    expect(screen.getByLabelText('Product').textContent).toBe('CNC')
    expect(await optionsOf(cnc.user, 'Product')).toEqual(['MIS', 'CNC', 'NRML'])
  })

  it.each([
    ['MARKET', false, false],
    ['LIMIT', true, false],
    ['SL', true, true],
    ['SL-M', false, true],
  ])('a %s leg shows Price %s and Trigger Price %s', (priceType, price, trigger) => {
    mount([{ ...ATM_CE, priceType }])
    expect(screen.queryByLabelText('Price', { exact: true }) !== null).toBe(price)
    expect(screen.queryByLabelText('Trigger Price') !== null).toBe(trigger)
  })

  it('follows the node price type for a leg with none of its own', () => {
    mount([ATM_CE], { commonPriceType: 'SL-M' })
    expect(screen.getByLabelText('Price type').textContent).toBe('Stop Loss Market')
    expect(screen.getByLabelText('Trigger Price')).toBeInTheDocument()
    expect(screen.queryByLabelText('Price', { exact: true })).toBeNull()
  })

  it('writes the price, trigger price and split size it is given', () => {
    mount([{ ...ATM_CE, priceType: 'SL', price: 10, triggerPrice: 9 }])
    fireEvent.change(screen.getByLabelText('Price', { exact: true }), { target: { value: '12' } })
    expect(saved[0].price).toBe(12)
    fireEvent.change(screen.getByLabelText('Trigger Price'), { target: { value: '11' } })
    expect(saved[0].triggerPrice).toBe(11)
    const split = screen.getByLabelText('Split Size')
    expect(split).toHaveAttribute('placeholder', '0 (no split)')
    fireEvent.change(split, { target: { value: '25' } })
    expect(saved[0].splitSize).toBe(25)
  })

  it.each([
    ['quantity', 'Quantity (Lots)', 'Quantity must be a number'],
    ['price', 'Price', 'SL needs a price above 0'],
    ['triggerPrice', 'Trigger Price', 'Trigger price must be a number'],
    ['splitSize', 'Split Size', 'Split size must be at least 0'],
  ])('ties the %s problem to its field by id', (field, label, problem) => {
    mount([
      { ...ATM_CE, quantity: 'abc', priceType: 'SL', price: 0, triggerPrice: '', splitSize: -1 },
    ])
    const input = screen.getByLabelText(label, { exact: true })
    expect(input.id).toBe(`custom-leg-0-${field}`)
    expect(input).toHaveAttribute('aria-invalid', 'true')
    expect(input).toHaveAttribute('aria-describedby', `custom-leg-0-${field}-error`)
    expect(document.getElementById(`custom-leg-0-${field}-error`)?.textContent).toBe(problem)
  })

  it('leaves a sound field unmarked', () => {
    mount()
    const quantity = screen.getByLabelText('Quantity (Lots)')
    expect(quantity).toHaveAttribute('aria-invalid', 'false')
    expect(quantity).not.toHaveAttribute('aria-describedby')
  })

  it('ties a typed strike and a typed expiry problem to their fields', async () => {
    getOptionStrikes.mockRejectedValue(new Error('no broker session'))
    const { user } = mount([{ ...ATM_CE, strikeMode: 'STRIKE', strike: 0, expiry: 'BAD' }])
    await screen.findByText(/Listed contracts unavailable/)
    const strike = screen.getByLabelText('Strike price')
    expect(strike).toHaveAttribute('aria-describedby', 'custom-leg-0-strike-error')
    expect(document.getElementById('custom-leg-0-strike-error')?.textContent).toBe(
      'Strike must be greater than 0'
    )

    await user.click(screen.getByRole('button', { name: 'Type a date or {{variable}}' }))
    const expiry = screen.getByLabelText('Expiry date')
    expect(expiry).toHaveAttribute('aria-invalid', 'true')
    expect(expiry).toHaveAttribute('aria-describedby', 'custom-leg-0-expiry-error')
    expect(document.getElementById('custom-leg-0-expiry-error')?.textContent).toBe(
      'Use DDMMMYY, e.g. 28OCT25'
    )
  })
})

describe('F-658 the contract hint', () => {
  it('says it is loading while the contracts are looked up', async () => {
    getOptionStrikes.mockReturnValue(new Promise(() => {}))
    mount()
    expect(await screen.findByText('Loading listed contracts...')).toBeInTheDocument()
  })

  it('explains in full what happens when the contracts cannot be listed', async () => {
    getOptionStrikes.mockRejectedValue(new Error('no broker session'))
    mount()
    expect(
      await screen.findByText(
        'Listed contracts unavailable, so strike and expiry are typed here. They are checked when the workflow is saved.'
      )
    ).toBeInTheDocument()
  })

  it('names the underlying, its price and the ATM strike', async () => {
    mount()
    expect(await screen.findByText('NIFTY 24219.05 - ATM 24200')).toBeInTheDocument()
  })

  it('falls back to "Underlying" and drops the ATM part when either is missing', async () => {
    getOptionStrikes.mockResolvedValue({ ...LISTING, underlyingSymbol: null, atm: null })
    mount()
    expect(await screen.findByText('Underlying 24219.05')).toBeInTheDocument()
  })

  it('shows no price line when the underlying has no price', async () => {
    getOptionStrikes.mockResolvedValue({ ...LISTING, underlyingLtp: null })
    mount()
    await waitFor(() => expect(screen.getByLabelText('Expiry').textContent).toContain('25AUG26'))
    expect(screen.queryByText(/24219.05/)).toBeNull()
  })
})
