/**
 * The Indicator node's config section: the indicator picker and the params it
 * prunes on a change, nesting on another indicator, the symbol block it
 * replaces, the params editor's remount, and the bar fields with their
 * fallbacks and help. Pins parity F-565 to F-568 and IND-082.
 *
 * Help strings are the source's own; the Value N Bars Back wording is the
 * desktop's (IND-083), the rest is the web's.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { INDICATOR_CATALOG } from '@/lib/flow/constants'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
}))

const { ConfigPanel } = await import('./ConfigPanel')

type Data = Record<string, unknown>

/** Opens the panel on the first node; the rest are there to switch to. */
function openPanel(...datas: Data[]) {
  useFlowWorkflowStore.setState({
    nodes: datas.map((data, i) => ({
      id: `n${i + 1}`,
      type: 'indicator',
      position: { x: 0, y: 0 },
      data,
    })),
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

/**
 * Opens the Select under `label` and reads its choices as shown. fireEvent, as
 * DefaultProduct.test.tsx does: the catalog has over a hundred entries, and
 * userEvent's pointer walk over them takes seconds per test.
 */
function openSelect(label: string): string[] {
  fireEvent.click(under(label, 'button'))
  const options = screen.getByRole('listbox').querySelectorAll('[role="option"]')
  return Array.from(options, (o) => o.textContent ?? '')
}

function pick(option: string) {
  fireEvent.click(within(screen.getByRole('listbox')).getByText(option))
}

function chooseIndicator(option: string) {
  openSelect('Indicator')
  pick(option)
}

const RSI = { symbol: 'SBIN', exchange: 'NSE', indicatorName: 'rsi' }

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('the indicator picker (F-565, IND-082)', () => {
  it('shows RSI when no indicator is stored', () => {
    openPanel({ symbol: 'SBIN' })
    expect(shownUnder('Indicator')).toBe('RSI (Momentum)')
  })

  it('lists the whole catalog as label (category) in a capped list', () => {
    openPanel(RSI)
    const shown = openSelect('Indicator')
    expect(shown).toEqual(INDICATOR_CATALOG.map((i) => `${i.label} (${i.category})`))
    expect(shown).toContain('MACD (Momentum)')
    expect(shown).toContain('Bbands (Volatility)')
    expect(document.querySelector('[data-slot="select-content"]')).toHaveClass('max-h-72')
  })

  it('stores the picked indicator', () => {
    openPanel(RSI)
    chooseIndicator('EMA (Trend)')
    expect(stored().indicatorName).toBe('ema')
  })
})

describe('params pruned on an indicator change (F-565, IND-082)', () => {
  it.each([
    ['keeps the keys the new indicator takes', 'SMA (Trend)', '{"period": 21}', '{"period":21}'],
    [
      'drops the keys it does not take',
      'MACD (Momentum)',
      '{"period": 14, "fast_period": 5}',
      '{"fast_period":5}',
    ],
    ['stores blank when nothing is left', 'ADL (Volume)', '{"period": 14}', ''],
    ['stores blank for blank params', 'SMA (Trend)', '', ''],
    [
      'keeps unparseable text as typed',
      'MACD (Momentum)',
      '{"period": {{p}}}',
      '{"period": {{p}}}',
    ],
    ['keeps a non-object as typed', 'MACD (Momentum)', '[14]', '[14]'],
  ])('%s', (_name, option, before, after) => {
    openPanel({ ...RSI, params: before })
    chooseIndicator(option)
    expect(stored().params).toBe(after)
  })
})

describe('nesting on another indicator (F-566, IND-082)', () => {
  const NEST_HELP =
    "Set to compute this indicator over another Indicator node's output (e.g. SMA of RSI) instead of fetching fresh history. Accepts a raw History array too - {{h.data}} uses each row's close. Only single-series indicators (SMA, EMA, RSI, WMA, stdev, highest/lowest, ...) can be nested."

  it('offers the nesting box with its help, and the symbol block while it is empty', () => {
    openPanel(RSI)
    const nest = inputUnder('Nest on another indicator (optional)')
    expect(nest).toHaveValue('')
    expect(nest).toHaveAttribute('placeholder', '{{rsi1.series}}')
    expect(screen.getByText(NEST_HELP)).toBeInTheDocument()
    expect(labelled('Source Field (optional)')).not.toBeInTheDocument()
    for (const label of ['Symbol', 'Exchange', 'Interval', 'Source', 'Lookback Bars']) {
      expect(labelled(label), label).toBeInTheDocument()
    }
  })

  it('swaps the symbol block for the source field once a series is set', () => {
    openPanel(RSI)
    fireEvent.change(inputUnder('Nest on another indicator (optional)'), {
      target: { value: '{{rsi1.series}}' },
    })
    expect(stored().sourceSeries).toBe('{{rsi1.series}}')
    for (const label of ['Symbol', 'Exchange', 'Interval', 'Source', 'Lookback Bars']) {
      expect(labelled(label), label).not.toBeInTheDocument()
    }
    const field = inputUnder('Source Field (optional)')
    expect(field).toHaveAttribute('placeholder', 'blank = auto (value, out0, close)')
    expect(
      screen.getByText('Which field to read from each row, e.g. high, low, out1.')
    ).toBeInTheDocument()
    fireEvent.change(field, { target: { value: 'out1' } })
    expect(stored().sourceField).toBe('out1')
  })

  it('brings the symbol block back when the series is cleared', () => {
    openPanel({ ...RSI, sourceSeries: '{{rsi1.series}}' })
    expect(labelled('Symbol')).not.toBeInTheDocument()
    fireEvent.change(inputUnder('Nest on another indicator (optional)'), {
      target: { value: '' },
    })
    expect(labelled('Symbol')).toBeInTheDocument()
    expect(labelled('Source Field (optional)')).not.toBeInTheDocument()
  })
})

describe('the symbol block (F-567, IND-082)', () => {
  it('asks for a symbol on NSE', () => {
    openPanel({ indicatorName: 'rsi' })
    const symbol = inputUnder('Symbol')
    expect(symbol).toHaveAttribute('placeholder', 'RELIANCE')
    fireEvent.change(symbol, { target: { value: 'INFY' } })
    expect(stored().symbol).toBe('INFY')
    expect(shownUnder('Exchange')).toBe('NSE')
  })

  it('shows a daily interval when none is stored and takes any interval as text', () => {
    openPanel({ indicatorName: 'rsi', symbol: 'SBIN' })
    const interval = inputUnder('Interval')
    expect(interval).toHaveValue('D')
    expect(interval).toHaveAttribute('placeholder', 'D, 5m, 1h, or a custom Historify interval')
    expect(
      screen.getByText(
        'Any interval your connected broker supports (check the Intervals node) - not a fixed list.'
      )
    ).toBeInTheDocument()
    fireEvent.change(interval, { target: { value: '2m' } })
    expect(stored().interval).toBe('2m')
  })

  it('reads from the broker by default and names the Historify intervals', () => {
    openPanel(RSI)
    expect(shownUnder('Source')).toBe('Broker API')
    expect(openSelect('Source')).toEqual([
      'Broker API',
      'Historify DB (custom intervals: 2m, 4m, W, M, Q)',
    ])
    pick('Historify DB (custom intervals: 2m, 4m, W, M, Q)')
    expect(stored().source).toBe('db')
  })

  it('looks back 100 bars by default, at least 5, and 100 again for blank', () => {
    openPanel(RSI)
    const lookback = inputUnder('Lookback Bars')
    expect(lookback).toHaveValue(100)
    expect(lookback).toHaveAttribute('min', '5')
    fireEvent.change(lookback, { target: { value: '250' } })
    expect(stored().lookbackBars).toBe(250)
    fireEvent.change(lookback, { target: { value: '' } })
    expect(stored().lookbackBars).toBe(100)
    expect(lookback).toHaveValue(100)
  })
})

describe('the params editor (F-568, IND-082)', () => {
  it('edits the stored params JSON text through fields', () => {
    openPanel({ ...RSI, params: '{"period": 21}' })
    const period = inputUnder('Period')
    expect(period).toHaveValue(21)
    expect(period).toHaveAttribute('placeholder', '14')
    fireEvent.change(period, { target: { value: '30' } })
    expect(stored().params).toBe('{"period":30}')
  })

  it('starts fresh for another indicator', () => {
    openPanel({ ...RSI, params: '{"period": 21}' })
    fireEvent.click(screen.getByRole('button', { name: 'Edit as JSON' }))
    expect(screen.getByPlaceholderText('{"period": 14}')).toBeInTheDocument()
    chooseIndicator('SMA (Trend)')
    expect(screen.getByRole('button', { name: 'Edit as JSON' })).toBeInTheDocument()
    expect(inputUnder('Period')).toHaveValue(21)
  })

  it('starts fresh for another node with the same indicator', () => {
    openPanel({ ...RSI, params: '{"period": 21}' }, { ...RSI, params: '{"period": 9}' })
    fireEvent.click(screen.getByRole('button', { name: 'Edit as JSON' }))
    act(() => useFlowWorkflowStore.setState({ selectedNodeId: 'n2' }))
    expect(screen.getByRole('button', { name: 'Edit as JSON' })).toBeInTheDocument()
    expect(inputUnder('Period')).toHaveValue(9)
  })
})

describe('the bar fields and the output (F-568, IND-082)', () => {
  it('reads the latest bar by default, 0 to 200, and 0 again for blank', () => {
    openPanel(RSI)
    const offset = inputUnder('Value N Bars Back')
    expect(offset).toHaveValue(0)
    expect(offset).toHaveAttribute('min', '0')
    expect(offset).toHaveAttribute('max', '200')
    expect(
      screen.getByText(
        '0 = latest bar, which may still be forming; 1 = the last closed bar. Read it via {{name.at_offset.value}} (or {{name.at_offset.out0}} for multi-output indicators). Prefer this over indexing {{name.series[N]}}, whose offsets shift with Tail Bars.'
      )
    ).toBeInTheDocument()
    fireEvent.change(offset, { target: { value: '3' } })
    expect(stored().offsetBars).toBe(3)
    fireEvent.change(offset, { target: { value: '' } })
    expect(stored().offsetBars).toBe(0)
  })

  it('keeps 5 tail bars by default, 1 to 200, and 5 again for blank', () => {
    openPanel(RSI)
    const tail = inputUnder('Tail Bars')
    expect(tail).toHaveValue(5)
    expect(tail).toHaveAttribute('min', '1')
    expect(tail).toHaveAttribute('max', '200')
    expect(
      screen.getByText(
        'Length of {{ind.series}} - a fixed-length recent-history array so {{ind.series[N]}} can address a specific historical bar.'
      )
    ).toBeInTheDocument()
    fireEvent.change(tail, { target: { value: '12' } })
    expect(stored().tailBars).toBe(12)
    fireEvent.change(tail, { target: { value: '' } })
    expect(stored().tailBars).toBe(5)
  })

  it('shows rsi1 as the output hint and says how to read the values', () => {
    openPanel(RSI)
    const output = inputUnder('Output Variable')
    expect(output).toHaveValue('')
    expect(output).toHaveAttribute('placeholder', 'rsi1')
    expect(
      screen.getByText(
        'Access with {{name.latest.value}}, {{name.previous.value}}, or {{name.series[N]}}. Multi-output indicators (MACD, BBands, ADX, ...) expose out0, out1, ...'
      )
    ).toBeInTheDocument()
    fireEvent.change(output, { target: { value: 'rsiFast' } })
    expect(stored().outputVariable).toBe('rsiFast')
  })
})
