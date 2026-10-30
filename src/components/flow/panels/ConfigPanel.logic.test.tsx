/**
 * The condition, gate and group nodes' config forms, as the web draws them:
 * time window, time condition, price condition, variable condition, position
 * check, fund check, the AND/OR/NOT gates and the group. Labels, defaults
 * shown, Select choices and what each stores, which fields each choice shows,
 * and the number fallbacks. Pins parity F-610 to F-618.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
}))

const { ConfigPanel } = await import('./ConfigPanel')

function openPanel(type: string, data: Record<string, unknown> = {}) {
  useFlowWorkflowStore.setState({
    nodes: [{ id: 'n1', type, position: { x: 0, y: 0 }, data }],
    selectedNodeId: 'n1',
  })
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })
  return render(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <ConfigPanel />
      </QueryClientProvider>
    </MemoryRouter>
  )
}

const stored = () => useFlowWorkflowStore.getState().nodes[0].data

/** The control drawn beside a label (the panel puts them side by side, not in <label for>). */
function under<K extends 'input' | 'button'>(label: string, tag: K): HTMLElementTagNameMap[K] {
  const el = screen.getByText(label, { selector: 'label' }).parentElement?.querySelector(tag)
  if (!el) throw new Error(`no ${tag} under ${label}`)
  return el
}

const inputUnder = (label: string) => under(label, 'input')
const shownUnder = (label: string) => under(label, 'button').textContent
const labelled = (label: string) => screen.queryByText(label, { selector: 'label' })

/** Opens the Select under `label` and reads its choices as shown. */
function openSelect(label: string): string[] {
  fireEvent.click(under(label, 'button'))
  const options = screen.getByRole('listbox').querySelectorAll('[role="option"]')
  return Array.from(options, (o) => o.textContent ?? '')
}

function pick(option: string) {
  fireEvent.click(within(screen.getByRole('listbox')).getByText(option))
}

/** The six comparison operators, '==' drawn as '='. */
const OPERATORS = ['>', '<', '=', '>=', '<=', '!=']

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('timeWindow (F-610)', () => {
  it('shows the 09:15 to 15:30 session when nothing is stored', () => {
    openPanel('timeWindow')
    expect(inputUnder('Start Time')).toHaveAttribute('type', 'time')
    expect(inputUnder('Start Time')).toHaveValue('09:15')
    expect(inputUnder('End Time')).toHaveAttribute('type', 'time')
    expect(inputUnder('End Time')).toHaveValue('15:30')
  })

  it('stores the times set', () => {
    openPanel('timeWindow')
    fireEvent.change(inputUnder('Start Time'), { target: { value: '10:00' } })
    fireEvent.change(inputUnder('End Time'), { target: { value: '14:00' } })
    expect(stored()).toMatchObject({ startTime: '10:00', endTime: '14:00' })
  })

  it('triggers inside the window until inverted', () => {
    openPanel('timeWindow')
    expect(screen.getByText('Invert Condition', { selector: 'label' })).toBeInTheDocument()
    expect(screen.getByText('Trigger outside window')).toBeInTheDocument()
    const invert = screen.getByRole('switch')
    expect(invert).not.toBeChecked()
    fireEvent.click(invert)
    expect(stored().invertCondition).toBe(true)
    expect(invert).toBeChecked()
  })
})

describe('timeCondition (F-611)', () => {
  const kind = (name: string) => screen.getByRole('button', { name })

  it('lights no condition type until one is stored', () => {
    openPanel('timeCondition')
    for (const name of ['Entry', 'Exit', 'Custom']) expect(kind(name)).toHaveClass('bg-muted')
  })

  it.each([
    ['Entry', 'entry', 'border-green-500'],
    ['Exit', 'exit', 'border-red-500'],
    ['Custom', 'custom', 'bg-primary'],
  ])('stores %s as %s and lights it', (name, value, lit) => {
    openPanel('timeCondition')
    fireEvent.click(kind(name))
    expect(stored().conditionType).toBe(value)
    expect(kind(name)).toHaveClass(lit)
  })

  it('compares at or after by default and offers five operators', () => {
    openPanel('timeCondition')
    expect(shownUnder('Operator')).toBe('At or After (>=)')
    expect(openSelect('Operator')).toEqual([
      'Equals (=)',
      'At or After (>=)',
      'At or Before (<=)',
      'After (>)',
      'Before (<)',
    ])
    pick('Before (<)')
    expect(stored().operator).toBe('<')
  })

  it('targets 09:30 when no time is stored and takes a label', () => {
    openPanel('timeCondition')
    const time = inputUnder('Target Time')
    expect(time).toHaveAttribute('type', 'time')
    expect(time).toHaveValue('09:30')
    fireEvent.change(time, { target: { value: '15:00' } })
    expect(stored().targetTime).toBe('15:00')
    const label = inputUnder('Label')
    expect(label).toHaveAttribute('placeholder', 'Market Open Entry')
    fireEvent.change(label, { target: { value: 'Exit window' } })
    expect(stored().label).toBe('Exit window')
  })
})

describe('priceCondition (F-612)', () => {
  it('asks for a symbol on NSE', () => {
    openPanel('priceCondition')
    const symbol = inputUnder('Symbol')
    expect(symbol).toHaveAttribute('placeholder', 'RELIANCE')
    fireEvent.change(symbol, { target: { value: 'SBIN' } })
    expect(stored().symbol).toBe('SBIN')
    expect(shownUnder('Exchange')).toBe('NSE')
  })

  it('compares the LTP by default and offers six fields', () => {
    openPanel('priceCondition', { symbol: 'SBIN' })
    expect(shownUnder('Field')).toBe('LTP')
    expect(openSelect('Field')).toEqual(['LTP', 'Open', 'High', 'Low', 'Prev Close', 'Change %'])
    pick('Change %')
    expect(stored().field).toBe('change_percent')
  })

  it('compares greater than by default, shows == as =, and stores ==', () => {
    openPanel('priceCondition', { symbol: 'SBIN' })
    expect(shownUnder('Operator')).toBe('>')
    expect(openSelect('Operator')).toEqual(OPERATORS)
    pick('=')
    expect(stored().operator).toBe('==')
  })

  it('takes a number in 0.05 steps, shows 0 as blank, and stores 0 for blank', () => {
    openPanel('priceCondition', { symbol: 'SBIN', value: 0 })
    const value = inputUnder('Value')
    expect(value).toHaveAttribute('type', 'number')
    expect(value).toHaveAttribute('step', '0.05')
    expect(value).toHaveValue(null)
    fireEvent.change(value, { target: { value: '812.35' } })
    expect(stored().value).toBe(812.35)
    fireEvent.change(value, { target: { value: '' } })
    expect(stored().value).toBe(0)
  })
})

describe('varCondition (F-613)', () => {
  it('compares two values with greater than by default', () => {
    openPanel('varCondition')
    expect(inputUnder('Left Value')).toHaveAttribute('placeholder', '{{rsi1.latest.value}}')
    expect(inputUnder('Right Value')).toHaveAttribute('placeholder', '30 or {{pdhpdl.pdh}}')
    expect(shownUnder('Operator')).toBe('>')
    expect(
      screen.getByText(
        'Compares any two values after {{...}} interpolation - an indicator output, a prior-period level, a workflow variable, or a literal number.'
      )
    ).toBeInTheDocument()
  })

  it('offers the six operators, == shown as =', () => {
    openPanel('varCondition')
    expect(openSelect('Operator')).toEqual(OPERATORS)
    pick('!=')
    expect(stored().operator).toBe('!=')
    openSelect('Operator')
    pick('=')
    expect(stored().operator).toBe('==')
  })

  it('stores both sides as typed', () => {
    openPanel('varCondition')
    fireEvent.change(inputUnder('Left Value'), { target: { value: '{{rsi1.latest.value}}' } })
    fireEvent.change(inputUnder('Right Value'), { target: { value: '70' } })
    expect(stored()).toMatchObject({ leftValue: '{{rsi1.latest.value}}', rightValue: '70' })
  })
})

describe('positionCheck (F-614)', () => {
  const THRESHOLD_CONDITIONS = ['quantity_above', 'quantity_below', 'pnl_above', 'pnl_below']

  it('asks for symbol, exchange and product, the product following the exchange', () => {
    openPanel('positionCheck', { exchange: 'NFO' })
    expect(inputUnder('Symbol')).toHaveAttribute('placeholder', 'RELIANCE')
    expect(shownUnder('Exchange')).toBe('NFO')
    expect(shownUnder('Product')).toBe('NRML')
    expect(stored().product).toBeUndefined()
  })

  it('checks that a position exists by default and offers six conditions', () => {
    openPanel('positionCheck', { symbol: 'SBIN' })
    expect(shownUnder('Condition')).toBe('Has Position')
    expect(openSelect('Condition')).toEqual([
      'Has Position',
      'No Position',
      'Qty Above',
      'Qty Below',
      'P&L Above',
      'P&L Below',
    ])
    pick('P&L Below')
    expect(stored().condition).toBe('pnl_below')
  })

  it.each([undefined, 'exists', 'not_exists'])('a %s condition asks for no threshold', (c) => {
    openPanel('positionCheck', { symbol: 'SBIN', condition: c })
    expect(labelled('Threshold')).not.toBeInTheDocument()
  })

  it.each(THRESHOLD_CONDITIONS)('a %s condition asks for a threshold', (condition) => {
    openPanel('positionCheck', { symbol: 'SBIN', condition })
    expect(inputUnder('Threshold')).toHaveAttribute('type', 'number')
  })

  it('shows the threshold once a threshold condition is picked', () => {
    openPanel('positionCheck', { symbol: 'SBIN' })
    openSelect('Condition')
    pick('Qty Above')
    expect(labelled('Threshold')).toBeInTheDocument()
  })

  it('stores the threshold as a number, 0 for blank, shown blank', () => {
    openPanel('positionCheck', { symbol: 'SBIN', condition: 'pnl_above', threshold: 0 })
    const threshold = inputUnder('Threshold')
    expect(threshold).toHaveValue(null)
    fireEvent.change(threshold, { target: { value: '1500.5' } })
    expect(stored().threshold).toBe(1500.5)
    fireEvent.change(threshold, { target: { value: '' } })
    expect(stored().threshold).toBe(0)
    expect(threshold).toHaveValue(null)
  })
})

describe('fundCheck (F-615)', () => {
  it('takes a minimum, 0 for blank, and says what it checks', () => {
    openPanel('fundCheck')
    const minimum = inputUnder('Minimum Available Funds')
    expect(minimum).toHaveAttribute('type', 'number')
    expect(minimum).toHaveAttribute('min', '0')
    expect(minimum).toHaveAttribute('placeholder', '10000')
    expect(minimum).toHaveValue(null)
    expect(screen.getByText('Checks if margin is above this')).toBeInTheDocument()
    fireEvent.change(minimum, { target: { value: '25000.5' } })
    expect(stored().minAvailable).toBe(25000.5)
    fireEvent.change(minimum, { target: { value: '' } })
    expect(stored().minAvailable).toBe(0)
  })
})

describe('andGate and orGate (F-616)', () => {
  it.each([
    ['andGate', 'AND Gate', 'Yes only if ALL conditions true'],
    ['orGate', 'OR Gate', 'Yes if ANY condition true'],
  ])('%s shows two inputs by default and says when it is true', (type, title, rule) => {
    openPanel(type)
    expect(shownUnder('Number of Inputs')).toBe('2 inputs')
    expect(screen.getByText(title, { selector: 'p' })).toBeInTheDocument()
    expect(screen.getByText(rule)).toBeInTheDocument()
  })

  it.each(['andGate', 'orGate'])('%s offers 2 to 5 inputs and stores a whole number', (type) => {
    openPanel(type)
    expect(openSelect('Number of Inputs')).toEqual(['2 inputs', '3 inputs', '4 inputs', '5 inputs'])
    pick('4 inputs')
    expect(stored().inputCount).toBe(4)
  })

  it('shows the stored input count', () => {
    openPanel('orGate', { inputCount: 3 })
    expect(shownUnder('Number of Inputs')).toBe('3 inputs')
  })
})

describe('notGate (F-617)', () => {
  it('only says what it does', () => {
    openPanel('notGate')
    expect(screen.getByText('NOT Gate', { selector: 'p' })).toBeInTheDocument()
    expect(
      screen.getByText('Inverts the condition. True becomes False, False becomes True.')
    ).toBeInTheDocument()
    expect(screen.queryAllByRole('textbox')).toHaveLength(0)
  })
})

describe('group (F-618)', () => {
  it('names the group', () => {
    openPanel('group')
    const name = inputUnder('Group Name')
    expect(name).toHaveAttribute('placeholder', 'Entry Logic')
    fireEvent.change(name, { target: { value: 'Exits' } })
    expect(stored().label).toBe('Exits')
  })

  it('uses the default colour until one is picked, and offers six', () => {
    openPanel('group')
    expect(shownUnder('Color')).toBe('Default')
    expect(openSelect('Color')).toEqual(['Default', 'Blue', 'Green', 'Red', 'Purple', 'Orange'])
    pick('Purple')
    expect(stored().color).toBe('purple')
  })
})
