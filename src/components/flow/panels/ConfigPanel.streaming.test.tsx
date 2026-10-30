/**
 * The streaming nodes' config forms, as the web draws them: the symbol and
 * exchange each subscribes to, the hint naming what it streams into, and the
 * Unsubscribe node's stream choices. Pins parity F-590 to F-593; the output
 * boxes' stored-value rule (F-585) is pinned in ConfigPanel.data.test.tsx.
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

const shownUnder = (label: string) => under(label, 'button').textContent

/** Opens the Select under `label` and reads its choices as shown. */
function openSelect(label: string): string[] {
  fireEvent.click(under(label, 'button'))
  const options = screen.getByRole('listbox').querySelectorAll('[role="option"]')
  return Array.from(options, (o) => o.textContent ?? '')
}

function pick(option: string) {
  fireEvent.click(within(screen.getByRole('listbox')).getByText(option))
}

/** A symbol box with the RELIANCE hint on an NSE picker, and the hint below the output. */
function expectSubscribeForm(hint: string) {
  const symbol = under('Symbol', 'input')
  expect(symbol).toHaveAttribute('placeholder', 'RELIANCE')
  fireEvent.change(symbol, { target: { value: 'SBIN' } })
  expect(stored().symbol).toBe('SBIN')
  expect(shownUnder('Exchange')).toBe('NSE')
  expect(screen.getByText(hint)).toBeInTheDocument()
}

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('subscribeLtp (F-590)', () => {
  it('subscribes a symbol on NSE and names the live value', () => {
    openPanel('subscribeLtp')
    expectSubscribeForm('Real-time: {{ltp}}')
  })

  it('stores the exchange the trader picks', () => {
    openPanel('subscribeLtp', { symbol: 'SBIN' })
    openSelect('Exchange')
    pick('BSE')
    expect(stored().exchange).toBe('BSE')
  })
})

describe('subscribeQuote (F-591)', () => {
  it('subscribes a symbol on NSE and names the quote fields', () => {
    openPanel('subscribeQuote')
    expectSubscribeForm('Use {{quote.ltp}}, {{quote.open}}')
  })
})

describe('subscribeDepth (F-592)', () => {
  it('subscribes a symbol on NSE and names the best bid', () => {
    openPanel('subscribeDepth')
    expectSubscribeForm('Use {{depth.bids[0].price}}')
  })
})

describe('unsubscribe (F-593)', () => {
  it('stops all streams by default and offers each one', () => {
    openPanel('unsubscribe')
    expect(shownUnder('Stream Type')).toBe('All Streams')
    expect(openSelect('Stream Type')).toEqual([
      'LTP Only',
      'Quote Only',
      'Depth Only',
      'All Streams',
    ])
    pick('Quote Only')
    expect(stored().streamType).toBe('quote')
  })

  it('takes every symbol when the symbol is blank, on NSE', () => {
    openPanel('unsubscribe')
    const symbol = under('Symbol', 'input')
    expect(symbol).toHaveValue('')
    expect(symbol).toHaveAttribute('placeholder', 'Empty = all symbols')
    fireEvent.change(symbol, { target: { value: 'SBIN' } })
    expect(stored().symbol).toBe('SBIN')
    expect(shownUnder('Exchange')).toBe('NSE')
  })
})

describe('F-593: the LTP and Depth stream choices', () => {
  it.each([
    ['LTP Only', 'ltp'],
    ['Depth Only', 'depth'],
    ['All Streams', 'all'],
  ])('stores %s as %s', (label, value) => {
    openPanel('unsubscribe', { streamType: 'quote' })
    openSelect('Stream Type')
    pick(label)
    expect(stored().streamType).toBe(value)
  })

  it.each([
    ['ltp', 'LTP Only'],
    ['quote', 'Quote Only'],
    ['depth', 'Depth Only'],
  ])('a stored %s shows %s', (streamType, shown) => {
    openPanel('unsubscribe', { streamType })
    expect(shownUnder('Stream Type')).toBe(shown)
  })
})
