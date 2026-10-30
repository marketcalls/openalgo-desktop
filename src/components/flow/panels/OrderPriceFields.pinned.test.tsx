/**
 * Pins the parts of parity F-523 the carried OrderPriceFields.test.tsx leaves
 * open: the 0.05 step on both boxes, each box named by its own label, and the
 * Braces toggles that seed {{webhook.price}} and {{webhook.triggerPrice}} and
 * fall back to 0.
 */

import { fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { describe, expect, it, vi } from 'vitest'
import type { PriceType } from '@/lib/flow/constants'
import { OrderPriceFields } from './OrderPriceFields'

/** Both prices held as the config panel holds them, with every write recorded. */
function Controlled({
  priceType,
  price: initialPrice = 0,
  triggerPrice: initialTrigger = 0,
  onPrice,
  onTrigger,
}: {
  priceType: PriceType
  price?: unknown
  triggerPrice?: unknown
  onPrice: (value: string | number) => void
  onTrigger: (value: string | number) => void
}) {
  const [price, setPrice] = useState<unknown>(initialPrice)
  const [triggerPrice, setTriggerPrice] = useState<unknown>(initialTrigger)
  return (
    <OrderPriceFields
      priceType={priceType}
      price={price}
      triggerPrice={triggerPrice}
      onPriceChange={(value) => {
        onPrice(value)
        setPrice(value)
      }}
      onTriggerPriceChange={(value) => {
        onTrigger(value)
        setTriggerPrice(value)
      }}
    />
  )
}

function mount(priceType: PriceType, values: { price?: unknown; triggerPrice?: unknown } = {}) {
  const onPrice = vi.fn()
  const onTrigger = vi.fn()
  render(<Controlled priceType={priceType} onPrice={onPrice} onTrigger={onTrigger} {...values} />)
  return { onPrice, onTrigger }
}

describe('the price boxes (F-523)', () => {
  it('steps both boxes by 0.05 and names each by its own label', () => {
    mount('SL', { price: 101.5, triggerPrice: 100.25 })

    const price = screen.getByLabelText('Price')
    const trigger = screen.getByLabelText('Trigger Price')
    expect(price).toHaveAttribute('type', 'number')
    expect(price).toHaveAttribute('step', '0.05')
    expect(price).toHaveValue(101.5)
    expect(trigger).toHaveAttribute('type', 'number')
    expect(trigger).toHaveAttribute('step', '0.05')
    expect(trigger).toHaveValue(100.25)
    expect(price.id).not.toBe('')
    expect(price.id).not.toBe(trigger.id)
  })

  it('gives each mounted form its own ids', () => {
    mount('LIMIT')
    mount('LIMIT')
    const [first, second] = screen.getAllByLabelText('Price')
    expect(first.id).not.toBe(second.id)
  })
})

describe('the Price toggle (F-523)', () => {
  it('seeds {{webhook.price}} and keeps the reference named by the label', () => {
    const { onPrice } = mount('LIMIT', { price: 101.5 })

    fireEvent.click(screen.getByRole('button', { name: 'Take Price from the payload' }))

    expect(onPrice).toHaveBeenLastCalledWith('{{webhook.price}}')
    const input = screen.getByLabelText('Price')
    expect(input).toHaveValue('{{webhook.price}}')
    expect(input).toHaveAttribute('placeholder', '{{webhook.price}}')
  })

  it('falls back to 0 when switched to a fixed price', () => {
    const { onPrice } = mount('LIMIT', { price: '{{webhook.price}}' })

    fireEvent.click(screen.getByRole('button', { name: 'Use a fixed Price' }))

    expect(onPrice).toHaveBeenLastCalledWith(0)
    expect(screen.getByLabelText('Price')).toHaveValue(0)
  })

  it('passes a typed reference through as text, not as a number', () => {
    const { onPrice } = mount('LIMIT', { price: '{{webhook.price}}' })

    fireEvent.change(screen.getByLabelText('Price'), { target: { value: '{{quote.ltp}}' } })

    expect(onPrice).toHaveBeenLastCalledWith('{{quote.ltp}}')
  })
})

describe('the Trigger Price toggle (F-523)', () => {
  it('seeds {{webhook.triggerPrice}}', () => {
    const { onTrigger, onPrice } = mount('SL-M', { triggerPrice: 99 })

    fireEvent.click(screen.getByRole('button', { name: 'Take Trigger Price from the payload' }))

    expect(onTrigger).toHaveBeenLastCalledWith('{{webhook.triggerPrice}}')
    expect(onPrice).not.toHaveBeenCalled()
    expect(screen.getByLabelText('Trigger Price')).toHaveAttribute(
      'placeholder',
      '{{webhook.triggerPrice}}'
    )
  })

  it('falls back to 0 when switched to a fixed trigger price', () => {
    const { onTrigger } = mount('SL', { triggerPrice: '{{webhook.triggerPrice}}' })

    fireEvent.click(screen.getByRole('button', { name: 'Use a fixed Trigger Price' }))

    expect(onTrigger).toHaveBeenLastCalledWith(0)
    expect(screen.getByLabelText('Trigger Price')).toHaveValue(0)
  })
})
