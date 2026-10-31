/**
 * The Margin Calculator's basket editor: the quantity label of a derivative
 * leg that cannot be shown in lots, and what the lot-size lookup asks for and
 * when (parity F-663, F-665): the 350 ms wait after a contract changes, how
 * long an answer is kept, the 50-contract cap and BFO contracts. A symbol is
 * entered in one change, the way a paste lands, never key by key: on a loaded
 * machine the gap between keys passed the debounce
 * (MarginPositionsFields.test.tsx).
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { useState } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { LotSizeMap } from '@/api/flow'

type Ref = { symbol: string; exchange: string }

const getSymbolLotSizes = vi.fn<(refs: Ref[]) => Promise<LotSizeMap>>()

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return { ...actual, getSymbolLotSizes: (refs: Ref[]) => getSymbolLotSizes(refs) }
})

const { MarginPositionsFields } = await import('./MarginPositionsFields')

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

function renderEditor(initial: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })
  const onChange = vi.fn()
  render(
    <QueryClientProvider client={client}>
      <Controlled initial={initial} onChange={onChange} />
    </QueryClientProvider>
  )
  return { client, onChange }
}

const lastWrite = (onChange: ReturnType<typeof vi.fn>) =>
  JSON.parse(onChange.mock.calls.at(-1)?.[0] as string) as Array<Record<string, unknown>>

const leg = (overrides: Record<string, unknown> = {}) => ({
  symbol: 'NIFTY25AUG26FUT',
  exchange: 'NFO',
  action: 'BUY',
  quantity: '130',
  product: 'NRML',
  pricetype: 'MARKET',
  price: '0',
  ...overrides,
})

const basket = (...legs: Array<Record<string, unknown>>) => JSON.stringify(legs)

/** A paragraph matched on its whole text, across the spans inside it. */
const paragraph = (text: string) =>
  screen.getByText((_, el) => el?.tagName === 'P' && el.textContent === text)

beforeEach(() => {
  getSymbolLotSizes.mockReset()
  getSymbolLotSizes.mockResolvedValue({ 'NFO:NIFTY25AUG26FUT': 65 })
})

describe('F-663 a derivative quantity that cannot be shown in lots reads "Quantity"', () => {
  /** The unit box: labelled "Quantity", with no lots box beside it. */
  function unitBox(): HTMLInputElement {
    expect(screen.queryByLabelText('Quantity (Lots)')).toBeNull()
    return screen.getByLabelText('Quantity') as HTMLInputElement
  }

  it('when the stored units are not whole lots, and it then writes units as typed', async () => {
    const { onChange } = renderEditor(basket(leg({ quantity: '100' })))
    await screen.findByText('100 units is not a multiple of the 65-unit lot.')
    const quantity = unitBox()
    expect(quantity.value).toBe('100')

    fireEvent.change(quantity, { target: { value: '120' } })
    expect(lastWrite(onChange)[0].quantity).toBe('120')
  })

  it('when the contract has no lot size', async () => {
    getSymbolLotSizes.mockResolvedValue({ 'NFO:NIFTY25AUG26FUT': null })
    renderEditor(basket(leg()))
    await screen.findByText('No lot size is available for this contract; quantity is in units.')
    expect(unitBox().value).toBe('130')
  })

  it('when the lot-size lookup failed', async () => {
    getSymbolLotSizes.mockRejectedValue(new Error('no broker session'))
    renderEditor(basket(leg()))
    await screen.findByText('Lot-size lookup failed. Quantity is in units.')
    expect(unitBox().value).toBe('130')
  })
})

describe('F-665 the lookup waits until the contract has settled for 350 ms', () => {
  beforeEach(() => {
    vi.useFakeTimers()
    vi.setSystemTime(new Date('2026-10-29T06:30:00Z'))
  })

  afterEach(() => {
    vi.useRealTimers()
  })

  /** Advance the faked clock and let React and the query settle. */
  const advance = (ms: number) =>
    act(async () => {
      await vi.advanceTimersByTimeAsync(ms)
    })

  const symbolBox = () => screen.getByLabelText('Symbol')

  it('looks a saved basket up on opening, without waiting', async () => {
    renderEditor(basket(leg()))
    await advance(0)
    expect(getSymbolLotSizes).toHaveBeenCalledTimes(1)
  })

  it('looks a changed contract up at 350 ms, not at 349', async () => {
    renderEditor(basket(leg({ symbol: '' })))
    await advance(0)
    expect(getSymbolLotSizes).not.toHaveBeenCalled()

    fireEvent.change(symbolBox(), { target: { value: 'NIFTY25AUG26FUT' } })
    await advance(349)
    expect(getSymbolLotSizes).not.toHaveBeenCalled()
    await advance(1)
    expect(getSymbolLotSizes.mock.calls).toEqual([
      [[{ symbol: 'NIFTY25AUG26FUT', exchange: 'NFO' }]],
    ])
  })

  it('starts the wait again on every change, so only the settled contract is asked for', async () => {
    renderEditor(basket(leg({ symbol: '' })))
    fireEvent.change(symbolBox(), { target: { value: 'NIFTY' } })
    await advance(300)
    fireEvent.change(symbolBox(), { target: { value: 'NIFTY25AUG26FUT' } })
    await advance(349)
    expect(getSymbolLotSizes).not.toHaveBeenCalled()
    await advance(1)
    expect(getSymbolLotSizes.mock.calls).toEqual([
      [[{ symbol: 'NIFTY25AUG26FUT', exchange: 'NFO' }]],
    ])
  })
})

describe('F-665 what the lookup asks for and how long it keeps the answer', () => {
  it('keeps the lot sizes for an hour', async () => {
    const { client } = renderEditor(basket(leg()))
    await screen.findByLabelText('Quantity (Lots)')
    const queries = client.getQueryCache().findAll({ queryKey: ['flow', 'symbol-lotsizes'] })
    expect(queries).toHaveLength(1)
    expect(queries[0].observers[0]?.options.staleTime).toBe(60 * 60 * 1000)
  })

  it('looks up a BFO contract and shows its quantity in lots', async () => {
    getSymbolLotSizes.mockResolvedValue({ 'BFO:SENSEX25AUG26FUT': 20 })
    const { onChange } = renderEditor(
      basket(leg({ symbol: 'SENSEX25AUG26FUT', exchange: 'BFO', quantity: '40' }))
    )
    const quantity = (await screen.findByLabelText('Quantity (Lots)')) as HTMLInputElement
    expect(getSymbolLotSizes.mock.calls).toEqual([
      [[{ symbol: 'SENSEX25AUG26FUT', exchange: 'BFO' }]],
    ])
    expect(quantity.value).toBe('2')
    expect(paragraph('2 lots x 20 = 40 units')).toBeInTheDocument()

    fireEvent.change(quantity, { target: { value: '3' } })
    expect(lastWrite(onChange)[0].quantity).toBe('60')
  })

  it('asks for at most 50 contracts, the first 50 the basket names', async () => {
    const legs = Array.from({ length: 51 }, (_, i) =>
      leg({ symbol: `NIFTY${String(i).padStart(2, '0')}FUT` })
    )
    getSymbolLotSizes.mockResolvedValue({})
    renderEditor(basket(...legs))
    await waitFor(() => expect(getSymbolLotSizes).toHaveBeenCalledTimes(1))
    const asked = getSymbolLotSizes.mock.calls[0][0]
    expect(asked).toHaveLength(50)
    expect(asked.map((r) => r.symbol).sort()).toEqual(
      legs.slice(0, 50).map((l) => String(l.symbol))
    )
    expect(asked.every((r) => r.exchange === 'NFO')).toBe(true)
  }, 20000)
})
