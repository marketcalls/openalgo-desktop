/**
 * Pins parity F-517 to F-522: the six order pickers in OrderFields.tsx (the
 * web's file unchanged). For each: its label, the choices and their labels,
 * what a pick writes, the fallback and the suggested reference behind the
 * Braces toggle, and the hint shown while the field takes a reference.
 */

import { fireEvent, render, screen, within } from '@testing-library/react'
import { type ComponentType, useState } from 'react'
import { describe, expect, it, vi } from 'vitest'
import { EXPIRY_TYPES } from '@/lib/flow/constants'
import {
  ActionField,
  ExchangeField,
  ExpiryField,
  PriceTypeField,
  ProductField,
  QuantityField,
} from './OrderFields'

type Change = (value: string | number) => void
type FieldComponent = ComponentType<{ value: unknown; onChange: Change } & Record<string, unknown>>

/** A controlled field, as the config panel holds one: the store is the state. */
function Controlled({
  Field,
  initial,
  onChange,
  ...rest
}: {
  Field: FieldComponent
  initial: unknown
  onChange: Change
} & Record<string, unknown>) {
  const [value, setValue] = useState<unknown>(initial)
  return (
    <Field
      value={value}
      onChange={(next) => {
        onChange(next)
        setValue(next)
      }}
      {...rest}
    />
  )
}

function mount(Field: unknown, initial: unknown, rest: Record<string, unknown> = {}) {
  const onChange = vi.fn()
  render(
    <Controlled Field={Field as FieldComponent} initial={initial} onChange={onChange} {...rest} />
  )
  return onChange
}

/** The first button under a label: the Select trigger, never the Braces toggle. */
function trigger(label: string): HTMLElement {
  const button = screen.getByText(label).parentElement?.querySelector('button')
  if (!button) throw new Error(`no control under ${label}`)
  return button
}

function offered(label: string): string[] {
  fireEvent.click(trigger(label))
  return within(screen.getByRole('listbox'))
    .getAllByRole('option')
    .map((option) => option.textContent ?? '')
}

function choose(label: string, option: string) {
  fireEvent.click(trigger(label))
  fireEvent.click(within(screen.getByRole('listbox')).getByRole('option', { name: option }))
}

const toPayload = (label: string) =>
  fireEvent.click(screen.getByRole('button', { name: `Take ${label} from the payload` }))
const toFixed = (label: string) =>
  fireEvent.click(screen.getByRole('button', { name: `Use a fixed ${label}` }))

describe('ExchangeField (F-517)', () => {
  it('offers the fourteen exchanges in order', () => {
    mount(ExchangeField, 'NSE')
    expect(screen.getByText('Exchange').tagName).toBe('LABEL')
    expect(offered('Exchange')).toEqual([
      'NSE',
      'BSE',
      'NFO',
      'BFO',
      'CDS',
      'BCD',
      'MCX',
      'NCDEX',
      'NCO',
      'NSE_INDEX',
      'BSE_INDEX',
      'MCX_INDEX',
      'GLOBAL_INDEX',
      'CRYPTO',
    ])
  })

  it('writes the exchange picked', () => {
    const onChange = mount(ExchangeField, 'NSE')
    choose('Exchange', 'MCX')
    expect(onChange).toHaveBeenLastCalledWith('MCX')
    expect(trigger('Exchange')).toHaveTextContent('MCX')
  })

  it('takes {{webhook.exchange}} from the payload, with no hint, and falls back to NSE', () => {
    const onChange = mount(ExchangeField, 'BSE')
    toPayload('Exchange')
    expect(onChange).toHaveBeenLastCalledWith('{{webhook.exchange}}')
    expect(screen.getByRole('textbox')).toHaveValue('{{webhook.exchange}}')
    expect(screen.getByText('Exchange').parentElement?.querySelector('p')).toBeNull()

    toFixed('Exchange')
    expect(onChange).toHaveBeenLastCalledWith('NSE')
    expect(trigger('Exchange')).toHaveTextContent('NSE')
  })

  it('takes a caller label and fallback', () => {
    const onChange = mount(ExchangeField, '{{webhook.cal}}', { label: 'Calendar', fallback: 'MCX' })
    toFixed('Calendar')
    expect(onChange).toHaveBeenLastCalledWith('MCX')
  })
})

describe('ActionField (F-518)', () => {
  const button = (name: string) => screen.getByRole('button', { name })

  it('shows BUY and SELL with neither selected and writes nothing when no action is stored', () => {
    const onChange = mount(ActionField, undefined)
    expect(screen.getByText('Action').tagName).toBe('LABEL')
    for (const name of ['BUY', 'SELL']) {
      expect(button(name)).toHaveClass('border-border', 'bg-muted')
      expect(button(name).className).not.toMatch(/green|red/)
    }
    expect(onChange).not.toHaveBeenCalled()
  })

  it('writes the side clicked and marks BUY green, SELL red', () => {
    const onChange = mount(ActionField, undefined)
    fireEvent.click(button('SELL'))
    expect(onChange).toHaveBeenLastCalledWith('SELL')
    expect(button('SELL')).toHaveClass('bg-red-500/20', 'border-red-500', 'text-red-600')
    expect(button('BUY')).toHaveClass('bg-muted')

    fireEvent.click(button('BUY'))
    expect(onChange).toHaveBeenLastCalledWith('BUY')
    expect(button('BUY')).toHaveClass('bg-green-500/20', 'border-green-500', 'text-green-600')
    expect(button('SELL')).toHaveClass('bg-muted')
  })

  it('takes {{webhook.action}} from the payload, says it must be BUY or SELL, and falls back to BUY', () => {
    const onChange = mount(ActionField, 'SELL')
    expect(screen.queryByText('Must resolve to BUY or SELL.')).not.toBeInTheDocument()
    toPayload('Action')
    expect(onChange).toHaveBeenLastCalledWith('{{webhook.action}}')
    expect(screen.getByText('Must resolve to BUY or SELL.')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'BUY' })).not.toBeInTheDocument()

    toFixed('Action')
    expect(onChange).toHaveBeenLastCalledWith('BUY')
    expect(button('BUY')).toHaveClass('bg-green-500/20')
  })
})

describe('QuantityField (F-519)', () => {
  const box = () => screen.getByRole('spinbutton')
  const HINT = 'A field holding one whole token keeps its type, so this arrives as a number.'

  it('is a number box with a minimum of 1 by default', () => {
    mount(QuantityField, 5)
    expect(screen.getByText('Quantity').tagName).toBe('LABEL')
    expect(box()).toHaveValue(5)
    expect(box()).toHaveAttribute('min', '1')
  })

  it.each([
    ['7', 7],
    ['12.9', 12],
    ['', 1],
    ['0', 1],
  ])('with min 1, typing %j writes %s', (typed, written) => {
    const onChange = mount(QuantityField, 5)
    fireEvent.change(box(), { target: { value: typed } })
    expect(onChange).toHaveBeenLastCalledWith(written)
  })

  it.each([
    ['0', 0],
    ['', 0],
    ['3', 3],
  ])('with min 0, typing %j writes %s', (typed, written) => {
    const onChange = mount(QuantityField, 5, { min: 0 })
    expect(box()).toHaveAttribute('min', '0')
    fireEvent.change(box(), { target: { value: typed } })
    expect(onChange).toHaveBeenLastCalledWith(written)
  })

  it('takes {{webhook.quantity}} from the payload, says it arrives as a number, and falls back to 1', () => {
    const onChange = mount(QuantityField, 5)
    expect(screen.queryByText(HINT)).not.toBeInTheDocument()
    toPayload('Quantity')
    expect(onChange).toHaveBeenLastCalledWith('{{webhook.quantity}}')
    expect(screen.getByRole('textbox')).toHaveValue('{{webhook.quantity}}')
    expect(screen.getByText(HINT)).toBeInTheDocument()

    toFixed('Quantity')
    expect(onChange).toHaveBeenLastCalledWith(1)
    expect(box()).toHaveValue(1)
  })

  it('takes a caller label', () => {
    mount(QuantityField, 2, { label: 'Quantity (Lots)' })
    expect(
      screen.getByRole('button', { name: 'Take Quantity (Lots) from the payload' })
    ).toBeTruthy()
  })
})

describe('ProductField (F-520)', () => {
  it('offers MIS, CNC and NRML and writes the one picked', () => {
    const onChange = mount(ProductField, 'MIS')
    expect(screen.getByText('Product').tagName).toBe('LABEL')
    expect(offered('Product')).toEqual(['MIS', 'CNC', 'NRML'])
    fireEvent.click(screen.getByRole('option', { name: 'CNC' }))
    expect(onChange).toHaveBeenLastCalledWith('CNC')
  })

  it('takes {{webhook.product}} from the payload, with no hint, and falls back to MIS', () => {
    const onChange = mount(ProductField, 'NRML')
    toPayload('Product')
    expect(onChange).toHaveBeenLastCalledWith('{{webhook.product}}')
    expect(screen.getByText('Product').parentElement?.querySelector('p')).toBeNull()
    toFixed('Product')
    expect(onChange).toHaveBeenLastCalledWith('MIS')
    expect(trigger('Product')).toHaveTextContent('MIS')
  })
})

describe('PriceTypeField (F-521)', () => {
  const HINT = 'Must resolve to MARKET, LIMIT, SL or SL-M.'

  it('offers the four price types by their trader names', () => {
    mount(PriceTypeField, 'MARKET')
    expect(screen.getByText('Price Type').tagName).toBe('LABEL')
    expect(offered('Price Type')).toEqual([
      'Market',
      'Limit',
      'Stop Loss Limit',
      'Stop Loss Market',
    ])
  })

  it.each([
    ['Market', 'MARKET'],
    ['Limit', 'LIMIT'],
    ['Stop Loss Limit', 'SL'],
    ['Stop Loss Market', 'SL-M'],
  ])('shows %s for %s and writes it when picked', (label, value) => {
    const onChange = mount(PriceTypeField, value === 'LIMIT' ? 'MARKET' : 'LIMIT')
    choose('Price Type', label)
    expect(onChange).toHaveBeenLastCalledWith(value)
    expect(trigger('Price Type')).toHaveTextContent(label)
  })

  it('takes {{webhook.priceType}} from the payload, names the four values, and falls back to MARKET', () => {
    const onChange = mount(PriceTypeField, 'SL')
    expect(screen.queryByText(HINT)).not.toBeInTheDocument()
    toPayload('Price Type')
    expect(onChange).toHaveBeenLastCalledWith('{{webhook.priceType}}')
    expect(screen.getByText(HINT)).toBeInTheDocument()
    toFixed('Price Type')
    expect(onChange).toHaveBeenLastCalledWith('MARKET')
    expect(trigger('Price Type')).toHaveTextContent('Market')
  })
})

describe('ExpiryField (F-522)', () => {
  const HINT = 'A relative type, an exact date such as 28OCT25, or a reference.'
  const monthly = EXPIRY_TYPES.filter((e) => e.value.endsWith('_month'))

  it('offers the choices it is given', () => {
    mount(ExpiryField, 'current_week', { options: EXPIRY_TYPES })
    expect(screen.getByText('Expiry').tagName).toBe('LABEL')
    expect(offered('Expiry')).toEqual(['Current Week', 'Next Week', 'Current Month', 'Next Month'])
  })

  it('offers only the monthly choices when the caller filters them (MCX)', () => {
    mount(ExpiryField, 'current_month', { options: monthly })
    expect(offered('Expiry')).toEqual(['Current Month', 'Next Month'])
  })

  it('writes the expiry type picked', () => {
    const onChange = mount(ExpiryField, 'current_week', { options: EXPIRY_TYPES })
    choose('Expiry', 'Next Month')
    expect(onChange).toHaveBeenLastCalledWith('next_month')
  })

  it('takes {{webhook.expiry}} in a text box, says it also takes an exact date, and falls back to current_week', () => {
    const onChange = mount(ExpiryField, 'next_week', { options: EXPIRY_TYPES })
    expect(screen.queryByText(HINT)).not.toBeInTheDocument()
    toPayload('Expiry')
    expect(onChange).toHaveBeenLastCalledWith('{{webhook.expiry}}')
    expect(screen.getByText(HINT)).toBeInTheDocument()

    toFixed('Expiry')
    expect(onChange).toHaveBeenLastCalledWith('current_week')
    expect(trigger('Expiry')).toHaveTextContent('Current Week')
  })

  it('passes an exact date pasted into the text box through as it is', () => {
    const onChange = mount(ExpiryField, '{{webhook.expiry}}', { options: EXPIRY_TYPES })
    fireEvent.change(screen.getByRole('textbox'), { target: { value: '28OCT25' } })
    expect(onChange).toHaveBeenLastCalledWith('28OCT25')
  })
})
