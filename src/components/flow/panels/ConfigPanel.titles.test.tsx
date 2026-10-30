/**
 * Pins parity F-502 (the header title of every node type, verbatim from the
 * web's NODE_TITLES) and the fallbacks behind it: a type without a title
 * takes its NODE_DEFINITIONS label through getNodeInfo (F-503) and shows that
 * label and description above a Node Label field; a type unknown to both
 * shows its raw name.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { DEFAULT_NODE_DATA, NODE_DEFINITIONS } from '@/lib/flow/constants'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

/** A palette entry added after the panel's title map was written. */
const LATER_NODE = vi.hoisted(() => ({
  type: 'laterNode',
  label: 'Later Node',
  description: 'Added to the palette after the panel was written',
  category: 'utility' as const,
}))

vi.mock('@/lib/flow/constants', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/lib/flow/constants')>()
  return { ...actual, NODE_DEFINITIONS: { ...actual.NODE_DEFINITIONS, LATER: [LATER_NODE] } }
})

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
  getWebhookInfo: vi.fn(async () => null),
  getSymbolLotSizes: vi.fn(async () => ({})),
}))

vi.mock('@/api/tunnel', () => ({
  tunnelApi: { get: vi.fn(async () => ({ tunnel: { state: 'stopped', url: null } })) },
}))

const { ConfigPanel } = await import('./ConfigPanel')

const TITLES: Record<string, string> = {
  start: 'Schedule Trigger',
  priceAlert: 'Price Alert',
  webhookTrigger: 'Webhook Trigger',
  orderUpdateTrigger: 'Order Update Trigger',
  placeOrder: 'Place Order',
  smartOrder: 'Smart Order',
  optionsOrder: 'Options Order',
  optionsMultiOrder: 'Multi-Leg Options',
  basketOrder: 'Basket Order',
  splitOrder: 'Split Order',
  cancelOrder: 'Cancel Order',
  cancelAllOrders: 'Cancel All Orders',
  closePositions: 'Close Positions',
  modifyOrder: 'Modify Order',
  getQuote: 'Get Quote',
  getDepth: 'Get Depth',
  getOrderStatus: 'Order Status',
  openPosition: 'Open Position',
  history: 'History Data',
  indicator: 'Indicator',
  priorPeriodOhlc: 'Prior Period OHLC',
  strategyPnl: 'Strategy P&L',
  barOffset: 'Bar Offset',
  expiry: 'Get Expiry',
  calendar: 'Calendar',
  intervals: 'Intervals',
  multiQuotes: 'Multi Quotes',
  symbol: 'Symbol Info',
  optionSymbol: 'Option Symbol',
  orderBook: 'Order Book',
  tradeBook: 'Trade Book',
  positionBook: 'Position Book',
  syntheticFuture: 'Synthetic Future',
  optionChain: 'Option Chain',
  holidays: 'Holidays',
  timings: 'Market Timings',
  holdings: 'Holdings',
  funds: 'Funds',
  margin: 'Margin Calculator',
  delay: 'Delay',
  waitUntil: 'Wait Until',
  log: 'Log',
  telegramAlert: 'Telegram Alert',
  whatsappAlert: 'WhatsApp Alert',
  variable: 'Variable',
  mathExpression: 'Math Expression',
  httpRequest: 'HTTP Request',
  timeWindow: 'Time Window',
  timeCondition: 'Time Condition',
  priceCondition: 'Price Condition',
  varCondition: 'Var Condition',
  positionCheck: 'Position Check',
  fundCheck: 'Fund Check',
  andGate: 'AND Gate',
  orGate: 'OR Gate',
  notGate: 'NOT Gate',
  group: 'Group',
  subscribeLtp: 'Subscribe LTP',
  subscribeQuote: 'Subscribe Quote',
  subscribeDepth: 'Subscribe Depth',
  unsubscribe: 'Unsubscribe',
}

function openPanel(type: string | undefined, data: Record<string, unknown> = {}) {
  useFlowWorkflowStore.setState({
    nodes: [{ id: 'n1', type, position: { x: 0, y: 0 }, data }],
    selectedNodeId: 'n1',
  })
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })
  render(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <ConfigPanel />
      </QueryClientProvider>
    </MemoryRouter>
  )
}

const title = () => screen.getByRole('heading', { level: 2 }).textContent

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('the header title of every node type (F-502)', () => {
  it('has a title for every palette type, and only for those', () => {
    const palette = Object.values(NODE_DEFINITIONS)
      .flat()
      .map((d) => d.type)
      .filter((type) => type !== LATER_NODE.type)
    expect(Object.keys(TITLES).sort()).toEqual([...palette].sort())
    expect(Object.keys(TITLES)).toHaveLength(61)
  })

  it.each(Object.entries(TITLES))('%s is titled %s', (type, expected) => {
    const defaults = (DEFAULT_NODE_DATA as Record<string, Record<string, unknown>>)[type]
    openPanel(type, { ...defaults })
    expect(title()).toBe(expected)
    // A titled type has its own form, never the generic Node Label field.
    expect(screen.queryByText('Node Label')).not.toBeInTheDocument()
  })
})

describe('a type the title map does not name (F-502, F-503 getNodeInfo)', () => {
  it('takes the palette label and shows its description above a Node Label field', () => {
    openPanel(LATER_NODE.type)

    expect(title()).toBe('Later Node')
    expect(screen.getByText('Later Node', { selector: 'p' })).toBeInTheDocument()
    expect(screen.getByText(LATER_NODE.description)).toBeInTheDocument()

    const label = screen.getByText('Node Label').parentElement?.querySelector('input')
    if (!label) throw new Error('no Node Label input')
    expect(label).toHaveAttribute('placeholder', 'Enter label...')
    fireEvent.change(label, { target: { value: 'My step' } })
    expect(useFlowWorkflowStore.getState().nodes[0].data.label).toBe('My step')
  })

  it('shows the raw type, and no description, when the palette does not know it either', () => {
    openPanel('mysteryNode', { label: 'Imported' })

    expect(title()).toBe('mysteryNode')
    expect(screen.queryByText(LATER_NODE.description)).not.toBeInTheDocument()
    expect(screen.getByText('Node Label').parentElement?.querySelector('input')).toHaveValue(
      'Imported'
    )
  })

  it('names a node with no type unknown', () => {
    openPanel(undefined)
    expect(title()).toBe('unknown')
  })
})
