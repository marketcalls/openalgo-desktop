/**
 * Pins the config panel's equity order forms, the web's code unchanged except
 * the split-order fallbacks (F-586, quantity 1 and split size 10, the run's
 * own): parity F-540 (place order), F-541 (smart order), F-547 and F-548
 * (basket), F-549 (split), F-550 (cancel), F-551 (cancel all), F-552 (close
 * positions) and F-553 (modify). Labels, placeholders, hints and choices are
 * the source's own words; every edit is read back from the store.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
  getWebhookInfo: vi.fn(async () => null),
}))

const { ConfigPanel } = await import('./ConfigPanel')

function openPanel(type: string, data: Record<string, unknown>) {
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

const labelled = (label: string) => screen.getByText(label, { selector: 'label' })
const isShown = (label: string) => screen.queryByText(label, { selector: 'label' }) !== null

/** The input under a label the panel draws as a sibling, not a <label for>. */
function fieldUnder(label: string): HTMLInputElement {
  const input = labelled(label).parentElement?.querySelector('input')
  if (!input) throw new Error(`no input under ${label}`)
  return input
}

/** The first button under a label: a Select trigger, never the Braces toggle. */
function triggerUnder(label: string): HTMLElement {
  const button = labelled(label).parentElement?.querySelector('button')
  if (!button) throw new Error(`no control under ${label}`)
  return button
}

function offered(label: string): string[] {
  fireEvent.click(triggerUnder(label))
  const texts = within(screen.getByRole('listbox'))
    .getAllByRole('option')
    .map((option) => option.textContent ?? '')
  fireEvent.keyDown(screen.getByRole('listbox'), { key: 'Escape' })
  return texts
}

function choose(label: string, option: string) {
  fireEvent.click(triggerUnder(label))
  fireEvent.click(within(screen.getByRole('listbox')).getByRole('option', { name: option }))
}

/** The help line drawn right under a field. */
const hintUnder = (label: string) =>
  labelled(label).parentElement?.querySelector('p')?.textContent ?? null

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('placeOrder (F-540)', () => {
  const ORDER = { symbol: 'SBIN', exchange: 'NSE', action: 'BUY', quantity: 5 }

  it('lays out the order fields in the web order', () => {
    openPanel('placeOrder', ORDER)
    const labels = [...document.querySelectorAll('.space-y-4.p-4 label')].map((l) => l.textContent)
    expect(labels).toEqual([
      'Symbol',
      'Exchange',
      'Action',
      'Quantity',
      'Product',
      'Price Type',
      'Output Variable',
      'Node ID',
    ])
  })

  it('takes a {{reference}} typed straight into the plain Symbol box', () => {
    openPanel('placeOrder', { ...ORDER, symbol: '' })
    expect(fieldUnder('Symbol')).toHaveAttribute('placeholder', 'RELIANCE')
    fireEvent.change(fieldUnder('Symbol'), { target: { value: '{{webhook.symbol}}' } })
    expect(stored().symbol).toBe('{{webhook.symbol}}')
  })

  it('shows NSE, quantity 1 and Market when none is stored, and no side selected', () => {
    openPanel('placeOrder', { symbol: 'SBIN' })
    expect(triggerUnder('Exchange')).toHaveTextContent('NSE')
    expect(fieldUnder('Quantity')).toHaveValue(1)
    expect(fieldUnder('Quantity')).toHaveAttribute('min', '1')
    expect(triggerUnder('Price Type')).toHaveTextContent('Market')
    for (const side of ['BUY', 'SELL']) {
      expect(screen.getByRole('button', { name: side })).toHaveClass('bg-muted')
    }
    expect(stored()).toEqual({ symbol: 'SBIN' })
  })

  it('shows a stored quantity of 0 as 1', () => {
    openPanel('placeOrder', { ...ORDER, quantity: 0 })
    expect(fieldUnder('Quantity')).toHaveValue(1)
  })

  it('stores the side, quantity and product picked', () => {
    openPanel('placeOrder', ORDER)
    fireEvent.click(screen.getByRole('button', { name: 'SELL' }))
    fireEvent.change(fieldUnder('Quantity'), { target: { value: '25' } })
    choose('Product', 'CNC')
    expect(stored()).toMatchObject({ action: 'SELL', quantity: 25, product: 'CNC' })
  })

  it('shows Price for a limit order and both prices for a stop-loss limit', () => {
    openPanel('placeOrder', ORDER)
    expect(isShown('Price')).toBe(false)
    choose('Price Type', 'Limit')
    expect(stored().priceType).toBe('LIMIT')
    expect(isShown('Price')).toBe(true)
    expect(isShown('Trigger Price')).toBe(false)
    fireEvent.change(screen.getByLabelText('Price'), { target: { value: '801.5' } })
    expect(stored().price).toBe(801.5)

    choose('Price Type', 'Stop Loss Limit')
    expect(stored().priceType).toBe('SL')
    fireEvent.change(screen.getByLabelText('Trigger Price'), { target: { value: '800' } })
    expect(stored().triggerPrice).toBe(800)
  })

  it('names the order result orderResult and says how to read its id', () => {
    openPanel('placeOrder', ORDER)
    expect(fieldUnder('Output Variable')).toHaveAttribute('placeholder', 'orderResult')
    expect(hintUnder('Output Variable')).toBe('Use {{orderResult.orderid}}')
    fireEvent.change(fieldUnder('Output Variable'), { target: { value: 'entry' } })
    expect(stored().outputVariable).toBe('entry')
  })
})

describe('smartOrder (F-541)', () => {
  const HELP = 'Target position (positive=long, negative=short, 0=use quantity)'

  it('takes a quantity down to 0 and shows 1 when none is stored', () => {
    openPanel('smartOrder', { symbol: 'SBIN', exchange: 'NSE' })
    expect(fieldUnder('Quantity')).toHaveValue(1)
    expect(fieldUnder('Quantity')).toHaveAttribute('min', '0')
  })

  it('shows the position size, 0 when none is stored, with its help', () => {
    openPanel('smartOrder', { symbol: 'SBIN', exchange: 'NSE' })
    expect(fieldUnder('Position Size')).toHaveAttribute('type', 'number')
    expect(fieldUnder('Position Size')).toHaveValue(0)
    expect(hintUnder('Position Size')).toBe(HELP)
  })

  it.each([
    ['-5', -5],
    ['12', 12],
    ['3.9', 3],
    ['', 0],
  ])('typing %j into Position Size stores %s', (typed, value) => {
    openPanel('smartOrder', { symbol: 'SBIN', exchange: 'NSE', positionSize: 7 })
    fireEvent.change(fieldUnder('Position Size'), { target: { value: typed } })
    expect(stored().positionSize).toBe(value)
  })

  it('names the result smartResult, with no hint', () => {
    openPanel('smartOrder', { symbol: 'SBIN', exchange: 'NSE' })
    expect(fieldUnder('Output Variable')).toHaveAttribute('placeholder', 'smartResult')
    expect(hintUnder('Output Variable')).toBeNull()
  })

  it('carries the action, product and price fields of an order', () => {
    openPanel('smartOrder', { symbol: 'SBIN', exchange: 'NFO', priceType: 'SL-M' })
    expect(screen.getByRole('button', { name: 'BUY' })).toBeInTheDocument()
    expect(triggerUnder('Product')).toHaveTextContent('NRML')
    expect(triggerUnder('Price Type')).toHaveTextContent('Stop Loss Market')
    expect(isShown('Trigger Price')).toBe(true)
    expect(isShown('Price')).toBe(false)
  })
})

describe('basketOrder: name and orders (F-547)', () => {
  const NOTE =
    'This imported per-order list is preserved read-only, including product and price overrides. Converting to CSV keeps only symbol, exchange, action, and quantity so the rows can be edited here.'
  const textarea = () =>
    labelled('Orders (SYMBOL,EXCHANGE,ACTION,QTY)').parentElement?.querySelector(
      'textarea'
    ) as HTMLTextAreaElement

  it('stores the basket name typed', () => {
    openPanel('basketOrder', {})
    expect(fieldUnder('Basket Name')).toHaveAttribute('placeholder', 'Morning Portfolio')
    fireEvent.change(fieldUnder('Basket Name'), { target: { value: 'Opening' } })
    expect(stored().basketName).toBe('Opening')
  })

  it('takes the orders as editable CSV rows, with a three-row example', () => {
    openPanel('basketOrder', {})
    expect(textarea()).toHaveAttribute(
      'placeholder',
      'RELIANCE,NSE,BUY,10\nINFY,NSE,BUY,5\nSBIN,NSE,SELL,20'
    )
    expect(textarea()).toHaveClass('font-mono')
    expect(textarea()).not.toHaveAttribute('readonly')
    expect(screen.queryByText(NOTE)).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Convert imported orders to CSV' })).toBeNull()

    fireEvent.change(textarea(), { target: { value: 'SBIN,NSE,BUY,1\nTCS,NSE,SELL,2' } })
    expect(stored().orders).toBe('SBIN,NSE,BUY,1\nTCS,NSE,SELL,2')
  })

  it('explains an imported list before offering to convert it', () => {
    openPanel('basketOrder', {
      orders: [{ symbol: 'SBIN', exchange: 'NSE', action: 'BUY', quantity: 1 }],
    })
    expect(screen.getByText(NOTE)).toBeInTheDocument()
    expect(textarea()).toHaveAttribute('readonly')
  })

  it('names the exchanges a basket row takes', () => {
    openPanel('basketOrder', {})
    expect(
      screen.getByText('Supported exchanges: NSE, BSE, NFO, BFO, CDS, BCD, MCX, NCO')
    ).toBeInTheDocument()
  })
})

describe('basketOrder: product, pricing and output (F-548)', () => {
  it('offers By row exchange then MIS, CNC and NRML', () => {
    openPanel('basketOrder', { orders: 'SBIN,NSE,BUY,1' })
    expect(offered('Product')).toEqual(['By row exchange', 'MIS', 'CNC', 'NRML'])
  })

  it('stores a blanket product picked', () => {
    openPanel('basketOrder', { orders: 'SBIN,NSE,BUY,1' })
    choose('Product', 'CNC')
    expect(stored().product).toBe('CNC')
  })

  it('shows Market by default and the price box for a limit basket', () => {
    openPanel('basketOrder', { orders: 'SBIN,NSE,BUY,1' })
    expect(triggerUnder('Price Type')).toHaveTextContent('Market')
    expect(isShown('Price')).toBe(false)
    choose('Price Type', 'Limit')
    expect(stored().priceType).toBe('LIMIT')
    fireEvent.change(screen.getByLabelText('Price'), { target: { value: '99.95' } })
    expect(stored().price).toBe(99.95)
  })

  it('names the result basketResult and says how to read it', () => {
    openPanel('basketOrder', { orders: 'SBIN,NSE,BUY,1' })
    expect(fieldUnder('Output Variable')).toHaveAttribute('placeholder', 'basketResult')
    expect(hintUnder('Output Variable')).toBe('Use {{basketResult.results}} in other nodes')
  })
})

describe('splitOrder (F-549)', () => {
  const ORDER = { symbol: 'SBIN', exchange: 'NSE', action: 'BUY' }

  it('lays out the split fields in the web order', () => {
    openPanel('splitOrder', ORDER)
    const labels = [...document.querySelectorAll('.space-y-4.p-4 label')].map((l) => l.textContent)
    expect(labels).toEqual([
      'Symbol',
      'Exchange',
      'Action',
      'Total Quantity',
      'Split Size',
      'Product',
      'Price Type',
      'Output Variable',
      'Node ID',
    ])
    expect(fieldUnder('Symbol')).toHaveAttribute('placeholder', 'RELIANCE')
  })

  it('takes a total quantity of at least 1, plain number only', () => {
    openPanel('splitOrder', { ...ORDER, quantity: 100, splitSize: 30 })
    const total = fieldUnder('Total Quantity')
    expect(total).toHaveAttribute('type', 'number')
    expect(total).toHaveAttribute('min', '1')
    expect(total).toHaveValue(100)
    expect(fieldUnder('Split Size')).toHaveAttribute('min', '1')
    expect(
      screen.queryByRole('button', { name: 'Take Total Quantity from the payload' })
    ).toBeNull()
  })

  it.each([
    ['250', 250],
    ['', 1],
    ['0', 1],
  ])('typing %j into Total Quantity stores %s', (typed, value) => {
    openPanel('splitOrder', { ...ORDER, quantity: 100 })
    fireEvent.change(fieldUnder('Total Quantity'), { target: { value: typed } })
    expect(stored().quantity).toBe(value)
  })

  it.each([
    ['25', 25],
    ['', 10],
    ['0', 10],
  ])('typing %j into Split Size stores %s', (typed, value) => {
    openPanel('splitOrder', { ...ORDER, quantity: 100, splitSize: 30 })
    fireEvent.change(fieldUnder('Split Size'), { target: { value: typed } })
    expect(stored().splitSize).toBe(value)
  })

  it('counts the orders the split makes', () => {
    openPanel('splitOrder', { ...ORDER, quantity: 100, splitSize: 30 })
    expect(screen.getByText(/^Splits into\s+4\s+orders$/)).toBeInTheDocument()
  })

  it('carries the product and price fields and names the result splitResult', () => {
    openPanel('splitOrder', { ...ORDER, quantity: 100, priceType: 'LIMIT' })
    expect(triggerUnder('Product')).toHaveTextContent('MIS')
    expect(triggerUnder('Price Type')).toHaveTextContent('Limit')
    expect(isShown('Price')).toBe(true)
    expect(fieldUnder('Output Variable')).toHaveAttribute('placeholder', 'splitResult')
    expect(hintUnder('Output Variable')).toBe('Use {{splitResult.results}} in other nodes')
  })
})

describe('cancelOrder (F-550)', () => {
  it('takes the order id, suggesting the Place Order result', () => {
    openPanel('cancelOrder', {})
    expect(fieldUnder('Order ID')).toHaveAttribute('placeholder', '{{orderResult.orderid}}')
    expect(hintUnder('Order ID')).toBe('Use variable from Place Order')
    fireEvent.change(fieldUnder('Order ID'), { target: { value: '{{entry.orderid}}' } })
    expect(stored().orderId).toBe('{{entry.orderid}}')
  })
})

describe('cancelAllOrders (F-551)', () => {
  it('only says what it does', () => {
    openPanel('cancelAllOrders', {})
    expect(
      screen.getByText('Cancels all open orders. No configuration needed.')
    ).toBeInTheDocument()
    const body = document.querySelector('.space-y-4.p-4') as HTMLElement
    expect(body.querySelectorAll('input, textarea, [role="combobox"]')).toHaveLength(0)
  })
})

describe('closePositions (F-552)', () => {
  const INFO =
    'Leave Symbol blank to square off every open position. Set it to close only that position; Exchange and Product narrow it further.'

  it('says a blank symbol closes everything, and asks nothing else', () => {
    openPanel('closePositions', { symbol: '', exchange: 'NSE' })
    expect(screen.getByText(INFO)).toBeInTheDocument()
    expect(fieldUnder('Symbol')).toHaveAttribute('placeholder', 'Blank = close all positions')
    expect(isShown('Exchange')).toBe(false)
    expect(isShown('Product')).toBe(false)
  })

  it('offers Exchange and Product once a symbol is typed', () => {
    openPanel('closePositions', { symbol: '', exchange: 'NSE' })
    fireEvent.change(fieldUnder('Symbol'), { target: { value: 'SBIN' } })
    expect(stored().symbol).toBe('SBIN')
    expect(triggerUnder('Exchange')).toHaveTextContent('NSE')
    expect(triggerUnder('Product')).toHaveTextContent('MIS')
    choose('Product', 'CNC')
    expect(stored().product).toBe('CNC')
  })
})

describe('modifyOrder (F-553)', () => {
  const INFO =
    'Symbol, exchange, side and product are read from the live order, so anything left blank here stays as it is.'

  it('asks for the order id and says the rest is read from the order', () => {
    openPanel('modifyOrder', {})
    expect(fieldUnder('Order ID')).toHaveAttribute('placeholder', '{{orderResult.orderid}}')
    expect(screen.getByText(INFO)).toBeInTheDocument()
    fireEvent.change(fieldUnder('Order ID'), { target: { value: '2510280001' } })
    expect(stored().orderId).toBe('2510280001')
  })

  it('leaves the new price and quantity empty to keep them', () => {
    openPanel('modifyOrder', { orderId: '1' })
    const price = fieldUnder('New Price')
    const quantity = fieldUnder('New Quantity')
    expect(price).toHaveAttribute('type', 'number')
    expect(price).toHaveAttribute('step', '0.05')
    expect(price).toHaveAttribute('placeholder', 'Leave empty to keep')
    expect(price).toHaveValue(null)
    expect(quantity).toHaveAttribute('type', 'number')
    expect(quantity).toHaveAttribute('min', '1')
    expect(quantity).toHaveAttribute('placeholder', 'Leave empty to keep')
    expect(quantity).toHaveValue(null)
  })

  it('stores what is typed as text, and an emptied box as empty text', () => {
    openPanel('modifyOrder', { orderId: '1' })
    fireEvent.change(fieldUnder('New Price'), { target: { value: '101.5' } })
    fireEvent.change(fieldUnder('New Quantity'), { target: { value: '25' } })
    expect(stored()).toMatchObject({ newPrice: '101.5', newQuantity: '25' })

    fireEvent.change(fieldUnder('New Price'), { target: { value: '' } })
    expect(stored().newPrice).toBe('')
  })
})
