/**
 * Pins parity F-515 and F-516: a field is "dynamic" by its content alone, and
 * the Braces toggle swaps the picker for a text box seeded with the suggested
 * reference, then back to the fallback. TemplatableField.tsx is the web's file
 * unchanged; these are its labels, titles, DOM order and writes.
 */

import { fireEvent, render, screen } from '@testing-library/react'
import { useState } from 'react'
import { describe, expect, it, vi } from 'vitest'
import { isTemplateValue, TemplatableField } from './TemplatableField'

/** A controlled field, as the config panel holds one: the store is the state. */
function Harness({
  initial,
  onChange,
  placeholder,
  hint,
}: {
  initial: unknown
  onChange: (value: string | number) => void
  placeholder?: string
  hint?: string
}) {
  const [value, setValue] = useState<unknown>(initial)
  return (
    <TemplatableField
      label="Exchange"
      value={value}
      onChange={(next) => {
        onChange(next)
        setValue(next)
      }}
      fallback="NSE"
      placeholder={placeholder}
      hint={hint}
      htmlFor="exchange-field"
    >
      <button type="button">picker</button>
    </TemplatableField>
  )
}

const wrapper = () => screen.getByText('Exchange').parentElement as HTMLElement

describe('isTemplateValue (F-515)', () => {
  it.each([
    ['{{webhook.exchange}}', true],
    ['NSE {{suffix}}', true],
    ['{{', true],
    ['NSE', false],
    ['{ {webhook.exchange} }', false],
    ['', false],
  ])('reads %j as dynamic: %s', (value, expected) => {
    expect(isTemplateValue(value)).toBe(expected)
  })

  it.each([[5], [0], [null], [undefined], [['{{x}}']], [{ value: '{{x}}' }]])(
    'never treats a non-string (%j) as dynamic',
    (value) => {
      expect(isTemplateValue(value)).toBe(false)
    }
  )
})

describe('a fixed value (F-516)', () => {
  it('shows the picker, no text box, no hint, and offers to take the value from the payload', () => {
    render(<Harness initial="NSE" onChange={vi.fn()} hint="Must resolve to an exchange." />)

    expect(screen.getByRole('button', { name: 'picker' })).toBeInTheDocument()
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument()
    expect(screen.queryByText('Must resolve to an exchange.')).not.toBeInTheDocument()
    const toggle = screen.getByRole('button', { name: 'Take Exchange from the payload' })
    expect(toggle).toHaveAttribute('title', 'Take this value from the payload')
    expect(toggle).toHaveAttribute('type', 'button')
  })

  it('writes the suggested reference when switched to the payload', () => {
    const onChange = vi.fn()
    render(<Harness initial="NSE" onChange={onChange} placeholder="{{webhook.exchange}}" />)

    fireEvent.click(screen.getByRole('button', { name: 'Take Exchange from the payload' }))

    expect(onChange).toHaveBeenCalledTimes(1)
    expect(onChange).toHaveBeenCalledWith('{{webhook.exchange}}')
  })

  it('seeds {{webhook.field}} when the caller suggests no reference', () => {
    const onChange = vi.fn()
    render(<Harness initial="NSE" onChange={onChange} />)

    fireEvent.click(screen.getByRole('button', { name: 'Take Exchange from the payload' }))

    expect(onChange).toHaveBeenCalledWith('{{webhook.field}}')
    expect(screen.getByRole('textbox')).toHaveAttribute('placeholder', '{{webhook.field}}')
  })
})

describe('a dynamic value (F-516)', () => {
  it('shows a mono text box named by the label, with the reference and the hint', () => {
    render(
      <Harness
        initial="{{webhook.exchange}}"
        onChange={vi.fn()}
        placeholder="{{webhook.exchange}}"
        hint="Must resolve to an exchange."
      />
    )

    const input = screen.getByLabelText('Exchange')
    expect(input).toHaveValue('{{webhook.exchange}}')
    expect(input).toHaveAttribute('id', 'exchange-field')
    expect(input).toHaveAttribute('placeholder', '{{webhook.exchange}}')
    expect(input).toHaveClass('font-mono')
    expect(screen.queryByRole('button', { name: 'picker' })).not.toBeInTheDocument()
    expect(screen.getByText('Must resolve to an exchange.')).toBeInTheDocument()
    const toggle = screen.getByRole('button', { name: 'Use a fixed Exchange' })
    expect(toggle).toHaveAttribute('title', 'Pick a fixed value instead')
  })

  it('passes typed text through as it is', () => {
    const onChange = vi.fn()
    render(<Harness initial="{{webhook.exchange}}" onChange={onChange} />)

    fireEvent.change(screen.getByLabelText('Exchange'), { target: { value: '{{webhook.seg}}' } })

    expect(onChange).toHaveBeenLastCalledWith('{{webhook.seg}}')
  })

  it('writes the fallback and brings the picker back when switched to a fixed value', () => {
    const onChange = vi.fn()
    render(<Harness initial="{{webhook.exchange}}" onChange={onChange} hint="A hint." />)

    fireEvent.click(screen.getByRole('button', { name: 'Use a fixed Exchange' }))

    expect(onChange).toHaveBeenCalledWith('NSE')
    expect(screen.getByRole('button', { name: 'picker' })).toBeInTheDocument()
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument()
    expect(screen.queryByText('A hint.')).not.toBeInTheDocument()
  })

  it('shows no hint line when the field has none', () => {
    render(<Harness initial="{{webhook.exchange}}" onChange={vi.fn()} />)

    expect(wrapper().querySelector('p')).toBeNull()
  })
})

describe('the DOM order the panel tests rely on (F-516)', () => {
  it('puts the label first and the toggle last, so the first button under the label is the picker', () => {
    render(<Harness initial="NSE" onChange={vi.fn()} />)

    const box = wrapper()
    expect(box.firstElementChild?.tagName).toBe('LABEL')
    expect(box.firstElementChild).toHaveAttribute('for', 'exchange-field')
    expect(box.lastElementChild).toHaveAccessibleName('Take Exchange from the payload')
    expect(box.querySelector('button')).toHaveTextContent('picker')
    // The toggle is an icon, so it adds nothing to the wrapper's text.
    expect(box.textContent).toBe('Exchangepicker')
  })

  it('keeps the text box first under the label while dynamic', () => {
    render(<Harness initial="{{webhook.exchange}}" onChange={vi.fn()} />)

    const box = wrapper()
    expect(box.firstElementChild?.tagName).toBe('LABEL')
    expect(box.querySelector('input')).toHaveValue('{{webhook.exchange}}')
    expect(box.lastElementChild).toHaveAccessibleName('Use a fixed Exchange')
  })
})
