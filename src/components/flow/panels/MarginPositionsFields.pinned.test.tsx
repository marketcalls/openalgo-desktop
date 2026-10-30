/**
 * The Margin Calculator's basket editor, pinned as the web renders it (parity
 * F-661, F-662, F-663, F-664): the JSON/fields toggle with its titles, the
 * JSON box and its three help sentences, the empty state, Add Position and
 * its cap of 50, the basket problem list and footer, the lots draft that
 * never commits a partial entry, the Price and Trigger Price inputs, and the
 * lot hint's wording in every state. The carried web tests in
 * MarginPositionsFields.test.tsx cover lookup cost, lots arithmetic and the
 * exchange/product rule.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { LotSizeMap } from '@/api/flow'

const getSymbolLotSizes =
  vi.fn<(refs: { symbol: string; exchange: string }[]) => Promise<LotSizeMap>>()

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return {
    ...actual,
    getSymbolLotSizes: (refs: { symbol: string; exchange: string }[]) => getSymbolLotSizes(refs),
  }
})

const { MarginPositionsFields } = await import('./MarginPositionsFields')

function client() {
  return new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })
}

function renderEditor(value: string) {
  const onChange = vi.fn()
  const view = render(
    <QueryClientProvider client={client()}>
      <MarginPositionsFields value={value} onChange={onChange} />
    </QueryClientProvider>
  )
  return { onChange, ...view }
}

/** Feeds onChange back into value, the way ConfigPanel does. */
function Controlled({ initial, onChange }: { initial: string; onChange: (raw: string) => void }) {
  const [value, setValue] = useState(initial)
  return (
    <MarginPositionsFields
      value={value}
      onChange={(raw) => {
        onChange(raw)
        setValue(raw)
      }}
    />
  )
}

function renderControlled(initial: string) {
  const onChange = vi.fn()
  render(
    <QueryClientProvider client={client()}>
      <Controlled initial={initial} onChange={onChange} />
    </QueryClientProvider>
  )
  return { onChange }
}

const lastWrite = (onChange: ReturnType<typeof vi.fn>) =>
  JSON.parse(onChange.mock.calls.at(-1)?.[0] as string) as Array<Record<string, unknown>>

const leg = (overrides: Record<string, unknown> = {}) => ({
  symbol: 'SBIN',
  exchange: 'NSE',
  action: 'BUY',
  quantity: '10',
  product: 'MIS',
  pricetype: 'MARKET',
  price: '0',
  ...overrides,
})

const basket = (...legs: Array<Record<string, unknown>>) => JSON.stringify(legs)

const NFO_LEG = leg({
  symbol: 'NIFTY25AUG26FUT',
  exchange: 'NFO',
  quantity: '130',
  product: 'NRML',
})

/** A paragraph matched on its whole text, across the spans inside it. */
const paragraph = (text: string) =>
  screen.getByText((_, el) => el?.tagName === 'P' && el.textContent === text)

beforeEach(() => {
  getSymbolLotSizes.mockReset()
  getSymbolLotSizes.mockResolvedValue({ 'NFO:NIFTY25AUG26FUT': 65 })
})

describe('F-661 JSON mode and field mode', () => {
  it('heads the basket "Positions" and switches to JSON and back', async () => {
    renderEditor(basket(leg()))
    expect(screen.getByText('Positions')).toBeInTheDocument()
    const toggle = screen.getByRole('button', { name: 'Edit as JSON' })
    expect(toggle).toBeEnabled()
    expect(toggle).not.toHaveAttribute('title')

    await userEvent.click(toggle)
    expect(screen.getByLabelText('Positions JSON')).toBeInTheDocument()
    expect(screen.queryByLabelText('Symbol')).toBeNull()

    await userEvent.click(screen.getByRole('button', { name: 'Use fields' }))
    expect(screen.getByLabelText('Symbol')).toBeInTheDocument()
  })

  it('shows the JSON box with its example and the field-mode help sentence', async () => {
    renderEditor(basket(leg()))
    await userEvent.click(screen.getByRole('button', { name: 'Edit as JSON' }))
    const box = screen.getByLabelText('Positions JSON')
    expect(box).toHaveAttribute(
      'placeholder',
      '[{"symbol": "NIFTY25DEC25FUT", "exchange": "NFO", "action": "BUY", "quantity": "75", "product": "NRML", "pricetype": "MARKET", "price": "0"}]'
    )
    expect(box).toHaveAttribute('aria-invalid', 'false')
    expect(box).toHaveAttribute('aria-describedby', 'margin-json-help')
    const help = screen.getByText(
      'Every leg needs symbol, exchange, action, quantity, product and pricetype. Use this view for {{variable}} references, which are interpolated before the JSON is parsed.'
    )
    expect(help.id).toBe('margin-json-help')
    expect(help).toHaveClass('text-muted-foreground')
  })

  it('writes the JSON text as typed', () => {
    const { onChange } = renderEditor('[')
    fireEvent.change(screen.getByLabelText('Positions JSON'), { target: { value: '[]' } })
    expect(onChange).toHaveBeenCalledWith('[]')
  })

  it('locks malformed JSON in the JSON box, says why on the toggle, and shows the parse error', () => {
    const raw = '[{"symbol":]'
    renderEditor(raw)
    const toggle = screen.getByRole('button', { name: 'Use fields' })
    expect(toggle).toBeDisabled()
    expect(toggle).toHaveAttribute('title', 'Fix the JSON before switching to fields')
    expect(screen.getByLabelText('Positions JSON')).toHaveAttribute('aria-invalid', 'true')

    let parseError = ''
    try {
      JSON.parse(raw)
    } catch (e) {
      parseError = (e as Error).message
    }
    const help = screen.getByText(parseError)
    expect(help.id).toBe('margin-json-help')
    expect(help).toHaveClass('text-destructive')
  })

  it('locks a {{variable}} basket in the JSON box and says why, in full', () => {
    renderEditor(basket(leg({ quantity: '{{qty}}' })))
    const toggle = screen.getByRole('button', { name: 'Use fields' })
    expect(toggle).toBeDisabled()
    expect(toggle).toHaveAttribute(
      'title',
      'A basket using {{variable}} references can only be edited as JSON'
    )
    expect(
      screen.getByText(
        'This basket uses {{variable}} references, which are interpolated before the JSON is parsed at run time. Field editing is unavailable so the references are not lost.'
      )
    ).toHaveAttribute('id', 'margin-json-help')
  })
})

describe('F-662 the field list', () => {
  it('shows the empty state and the basket problem with no positions', () => {
    renderEditor('')
    expect(
      screen.getByText('No positions yet. Margin is estimated for the basket as a whole.')
    ).toBeInTheDocument()
    expect(screen.getByText('Add at least one position')).toHaveClass('text-destructive')
  })

  it('appends a default position after the ones already there', async () => {
    const { onChange } = renderEditor(basket(leg({ symbol: 'INFY' })))
    await userEvent.click(screen.getByRole('button', { name: 'Add Position' }))
    expect(lastWrite(onChange)).toEqual([
      leg({ symbol: 'INFY' }),
      {
        symbol: '',
        exchange: 'NSE',
        action: 'BUY',
        quantity: '1',
        product: 'MIS',
        pricetype: 'MARKET',
        price: '0',
      },
    ])
  })

  // A text query, not a role query: computing roles over 50 rows is slow.
  const addButton = () => screen.getByText('Add Position').closest('button')

  it('stops adding at 50 positions', () => {
    renderEditor(basket(...Array.from({ length: 50 }, () => leg())))
    expect(addButton()).toBeDisabled()
  }, 20000)

  it('still adds at 49', () => {
    renderEditor(basket(...Array.from({ length: 49 }, () => leg())))
    expect(addButton()).toBeEnabled()
  }, 20000)

  it('lists the basket problems as a destructive list', () => {
    renderEditor(basket(leg({ symbol: '' }), leg(), leg({ pricetype: 'LIMIT', price: '0' })))
    const item = screen.getByText('2 positions need attention')
    expect(item.tagName).toBe('LI')
    expect(item).toHaveClass('text-destructive')
  })

  it('counts a single position needing attention in the singular', () => {
    renderEditor(basket(leg({ symbol: '' })))
    expect(screen.getByText('1 position needs attention')).toBeInTheDocument()
  })

  it('lists nothing for a sound basket', () => {
    renderEditor(basket(leg()))
    expect(screen.queryByRole('list')).toBeNull()
  })

  it('ends with the 50-leg note', () => {
    renderEditor(basket(leg()))
    expect(
      screen.getByText(
        'Up to 50 legs. Margin is estimated for the basket as a whole, so offsetting legs get the hedged requirement.'
      )
    ).toBeInTheDocument()
  })
})

describe('F-663 the leg row', () => {
  it('labels the row, the symbol box and the action legend for its leg number', () => {
    renderEditor(basket(leg(), leg({ symbol: 'INFY' })))
    expect(screen.getByText('Leg 2')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Remove leg 2' })).toBeInTheDocument()
    expect(screen.getAllByLabelText('Symbol')[0]).toHaveAttribute('placeholder', 'NIFTY25DEC25FUT')
    const legend = screen.getByText('Action for leg 2')
    expect(legend.tagName).toBe('LEGEND')
    expect(legend).toHaveClass('sr-only')
  })

  it('writes the side chosen', async () => {
    const { onChange } = renderEditor(basket(leg()))
    await userEvent.click(screen.getByRole('button', { name: 'SELL' }))
    expect(lastWrite(onChange)[0].action).toBe('SELL')
  })

  it('offers MIS, CNC and NRML as products', async () => {
    renderEditor(basket(leg()))
    await userEvent.click(screen.getByLabelText('Product'))
    expect((await screen.findAllByRole('option')).map((o) => o.textContent)).toEqual([
      'MIS',
      'CNC',
      'NRML',
    ])
  })

  it('keeps a blank or zero lots entry as a draft and commits nothing until it is whole', async () => {
    const { onChange } = renderEditor(basket(NFO_LEG))
    const quantity = (await screen.findByLabelText('Quantity (Lots)')) as HTMLInputElement
    await waitFor(() => expect(quantity.value).toBe('2'))

    fireEvent.change(quantity, { target: { value: '' } })
    expect(quantity.value).toBe('')
    fireEvent.change(quantity, { target: { value: '0' } })
    expect(quantity.value).toBe('0')
    expect(onChange).not.toHaveBeenCalled()

    fireEvent.change(quantity, { target: { value: '4' } })
    expect(lastWrite(onChange)[0].quantity).toBe('260')
  })

  it('drops the draft on leaving the box, showing the stored lots again', async () => {
    renderEditor(basket(NFO_LEG))
    const quantity = (await screen.findByLabelText('Quantity (Lots)')) as HTMLInputElement
    await waitFor(() => expect(quantity.value).toBe('2'))
    fireEvent.change(quantity, { target: { value: '' } })
    fireEvent.blur(quantity)
    expect(quantity.value).toBe('2')
  })

  it('commits a unit quantity as typed when no lot size applies', () => {
    const { onChange } = renderEditor(basket(leg()))
    fireEvent.change(screen.getByLabelText('Quantity'), { target: { value: '' } })
    expect(lastWrite(onChange)[0].quantity).toBe('0')
    fireEvent.change(screen.getByLabelText('Quantity'), { target: { value: '25' } })
    expect(lastWrite(onChange)[0].quantity).toBe('25')
  })

  it.each([
    ['MARKET', false, false],
    ['LIMIT', true, false],
    ['SL', true, true],
    ['SL-M', false, true],
  ])('a %s leg shows Price %s and Trigger Price %s', (pricetype, price, trigger) => {
    renderEditor(basket(leg({ pricetype, price: '5', trigger_price: '4' })))
    expect(screen.queryByLabelText('Price', { exact: true }) !== null).toBe(price)
    expect(screen.queryByLabelText('Trigger Price') !== null).toBe(trigger)
  })

  it('takes the price and the trigger price as decimal numbers and writes them', () => {
    const { onChange } = renderEditor(
      basket(leg({ pricetype: 'SL', price: '5', trigger_price: '4' }))
    )
    const price = screen.getByLabelText('Price', { exact: true })
    const trigger = screen.getByLabelText('Trigger Price')
    for (const input of [price, trigger]) {
      expect(input).toHaveAttribute('type', 'number')
      expect(input).toHaveAttribute('step', 'any')
      expect(input).toHaveAttribute('min', '0')
    }
    expect(price.id).toBe('margin-leg-0-price')
    expect(trigger.id).toBe('margin-leg-0-trigger_price')

    fireEvent.change(price, { target: { value: '101.5' } })
    expect(lastWrite(onChange)[0].price).toBe('101.5')
    fireEvent.change(trigger, { target: { value: '100.25' } })
    expect(lastWrite(onChange)[0].trigger_price).toBe('100.25')
  })

  it('ties the price problems to their inputs by id', () => {
    renderEditor(basket(leg({ pricetype: 'SL', price: '0', trigger_price: '0' })))
    const price = screen.getByLabelText('Price', { exact: true })
    expect(price).toHaveAttribute('aria-invalid', 'true')
    expect(price).toHaveAttribute('aria-describedby', 'margin-leg-0-price-error')
    expect(document.getElementById('margin-leg-0-price-error')?.textContent).toBe(
      'SL needs a price above 0'
    )
    const trigger = screen.getByLabelText('Trigger Price')
    expect(trigger).toHaveAttribute('aria-describedby', 'margin-leg-0-trigger_price-error')
    expect(document.getElementById('margin-leg-0-trigger_price-error')?.textContent).toBe(
      'SL needs a trigger price above 0'
    )
  })

  it('ties the quantity problem to its input by id', () => {
    renderEditor(basket(leg({ quantity: '2.5' })))
    const quantity = screen.getByLabelText('Quantity')
    expect(quantity).toHaveAttribute('aria-describedby', 'margin-leg-0-quantity-error')
    expect(document.getElementById('margin-leg-0-quantity-error')?.textContent).toBe(
      'Quantity must be a positive whole number'
    )
  })

  it('switches the price type it is given', async () => {
    const { onChange } = renderControlled(basket(leg()))
    await userEvent.click(screen.getByLabelText('Price type'))
    await userEvent.click(await screen.findByRole('option', { name: 'Limit' }))
    expect(lastWrite(onChange)[0].pricetype).toBe('LIMIT')
    expect(await screen.findByLabelText('Price', { exact: true })).toBeInTheDocument()
  })
})

describe('F-664 the lot hint', () => {
  it('says it is looking up the lot size while the lookup runs', () => {
    getSymbolLotSizes.mockReturnValue(new Promise(() => {}))
    renderEditor(basket(NFO_LEG))
    expect(screen.getByText('Looking up lot size...')).toBeInTheDocument()
  })

  it('reads whole lots as "{lots} lots x {lotSize} = {units} units"', async () => {
    renderEditor(basket(NFO_LEG))
    await waitFor(() => expect(paragraph('2 lots x 65 = 130 units')).toBeInTheDocument())
  })

  it('reads a single lot in the singular', async () => {
    renderEditor(basket({ ...NFO_LEG, quantity: '65' }))
    await waitFor(() => expect(paragraph('1 lot x 65 = 65 units')).toBeInTheDocument())
  })

  it('offers to round an odd quantity to one lot in the singular', async () => {
    const { onChange } = renderEditor(basket({ ...NFO_LEG, quantity: '40' }))
    expect(await screen.findByText('40 units is not a multiple of the 65-unit lot.')).toHaveClass(
      'text-destructive'
    )
    await userEvent.click(screen.getByRole('button', { name: 'Round to 1 lot' }))
    expect(lastWrite(onChange)[0].quantity).toBe('65')
  })

  it('says a failed lookup leaves the quantity in units and retries on request', async () => {
    getSymbolLotSizes.mockRejectedValue(new Error('no broker session'))
    renderEditor(basket(NFO_LEG))
    const failed = await screen.findByText('Lot-size lookup failed. Quantity is in units.')
    expect(failed).toHaveClass('text-destructive')
    expect(getSymbolLotSizes).toHaveBeenCalledTimes(1)

    const row = failed.parentElement as HTMLElement
    await userEvent.click(within(row).getByRole('button', { name: 'Retry' }))
    await waitFor(() => expect(getSymbolLotSizes).toHaveBeenCalledTimes(2))
  })

  it('says, in full, when the contract has no lot size', async () => {
    getSymbolLotSizes.mockResolvedValue({ 'NFO:NIFTY25AUG26FUT': null })
    renderEditor(basket(NFO_LEG))
    expect(
      await screen.findByText('No lot size is available for this contract; quantity is in units.')
    ).toBeInTheDocument()
  })

  it('shows no hint for a leg outside NFO and BFO', () => {
    renderEditor(basket(leg()))
    expect(screen.queryByText('Looking up lot size...')).toBeNull()
    expect(screen.queryByText(/lot/)).toBeNull()
  })
})
