/**
 * The node palette a trader builds from (parity F-630, F-632..F-637, F-697,
 * X-261). NodePalette.tsx keeps its own six lists rather than reading
 * NODE_DEFINITIONS, so they are pinned here as the trader sees them: the
 * tabs in order, every entry's label and description, the type each one adds
 * or drags, and together exactly the node types the backend's validator
 * accepts (src-tauri/src/flow/validator/catalog.rs VALID_NODE_TYPES) and the
 * canvas can draw.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fireEvent, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { nodeTypes } from '@/components/flow/nodes'
import { NodePalette } from './NodePalette'

type Entry = [type: string, label: string, description: string]

const TABS: Array<[tab: string, entries: Entry[]]> = [
  [
    'Trigger',
    [
      ['start', 'Schedule', 'Start on schedule'],
      ['priceAlert', 'Price Alert', 'Trigger on price'],
      ['webhookTrigger', 'Webhook', 'External trigger'],
      ['orderUpdateTrigger', 'Order Update', 'Trigger on fill/reject'],
    ],
  ],
  [
    'Action',
    [
      ['placeOrder', 'Place Order', 'Basic order'],
      ['smartOrder', 'Smart Order', 'Position-aware'],
      ['optionsOrder', 'Options Order', 'ATM/ITM/OTM'],
      ['optionsMultiOrder', 'Multi-Leg', 'Options strategies'],
      ['basketOrder', 'Basket Order', 'Multiple orders'],
      ['splitOrder', 'Split Order', 'Large order split'],
      ['modifyOrder', 'Modify Order', 'Edit order'],
      ['cancelOrder', 'Cancel Order', 'Cancel by ID'],
      ['cancelAllOrders', 'Cancel All', 'Cancel orders'],
      ['closePositions', 'Close Positions', 'Square off all'],
    ],
  ],
  [
    'Data',
    [
      ['getQuote', 'Get Quote', 'Real-time quote'],
      ['getDepth', 'Get Depth', 'Bid/ask depth'],
      ['getOrderStatus', 'Order Status', 'Check order'],
      ['history', 'History', 'OHLCV data'],
      ['indicator', 'Indicator', 'Any technical indicator'],
      ['strategyPnl', 'Strategy P&L', 'Per-strategy realized/unrealized'],
      ['priorPeriodOhlc', 'Prior Period OHLC', 'Prev hour/day/week/month'],
      ['barOffset', 'Bar Offset', 'OHLCV N bars back'],
      ['openPosition', 'Open Position', 'Get position'],
      ['expiry', 'Expiry Dates', 'F&O expiry'],
      ['intervals', 'Intervals', 'Broker timeframes'],
      ['calendar', 'Calendar', 'New day / week / month'],
      ['multiQuotes', 'Multi Quotes', 'Multiple symbols'],
      ['symbol', 'Symbol Info', 'Get symbol details'],
      ['optionSymbol', 'Option Symbol', 'Resolve options'],
      ['orderBook', 'Order Book', 'All orders'],
      ['tradeBook', 'Trade Book', 'Executed trades'],
      ['positionBook', 'Position Book', 'All positions'],
      ['syntheticFuture', 'Synthetic Future', 'Calc future price'],
      ['optionChain', 'Option Chain', 'Full chain data'],
      ['holdings', 'Holdings', 'Portfolio holdings'],
      ['funds', 'Funds', 'Account balance'],
      ['margin', 'Margin Calc', 'Margin required'],
    ],
  ],
  [
    'Stream',
    [
      ['subscribeLtp', 'Subscribe LTP', 'Live price stream'],
      ['subscribeQuote', 'Subscribe Quote', 'Live OHLC stream'],
      ['subscribeDepth', 'Subscribe Depth', 'Live order book'],
      ['unsubscribe', 'Unsubscribe', 'Stop streaming'],
    ],
  ],
  [
    'Logic',
    [
      ['timeCondition', 'Time Condition', 'Entry/Exit time'],
      ['positionCheck', 'Position Check', 'Check position'],
      ['fundCheck', 'Fund Check', 'Check funds'],
      ['priceCondition', 'Price Check', 'Check price'],
      ['timeWindow', 'Time Window', 'Market hours'],
      ['andGate', 'AND Gate', 'All must be true'],
      ['orGate', 'OR Gate', 'Any can be true'],
      ['notGate', 'NOT Gate', 'Invert condition'],
      ['varCondition', 'Var Condition', 'Compare any two values'],
    ],
  ],
  [
    'Util',
    [
      ['variable', 'Variable', 'Store values'],
      ['mathExpression', 'Math', 'Calculate expression'],
      ['log', 'Log', 'Debug message'],
      ['telegramAlert', 'Telegram', 'Send alert'],
      ['whatsappAlert', 'WhatsApp', 'Send alert'],
      ['delay', 'Delay', 'Wait duration'],
      ['waitUntil', 'Wait Until', 'Wait until time'],
      ['group', 'Group', 'Group nodes'],
      ['httpRequest', 'HTTP Request', 'API call'],
      ['holidays', 'Holidays', 'Market holidays'],
      ['timings', 'Timings', 'Market hours'],
    ],
  ],
]

/** The node types the backend's validator accepts, read from its source. */
function backendNodeTypes(): string[] {
  const catalog = readFileSync(
    join(process.cwd(), 'src-tauri/src/flow/validator/catalog.rs'),
    'utf8'
  )
  const start = catalog.indexOf('pub const VALID_NODE_TYPES')
  const block = catalog.slice(start, catalog.indexOf('];', start))
  return [...block.matchAll(/"(\w+)"/g)].map((m) => m[1])
}

function renderPalette() {
  const onAdd = vi.fn()
  const onDragStart = vi.fn()
  render(<NodePalette onAdd={onAdd} onDragStart={onDragStart} />)
  return { onAdd, onDragStart }
}

describe('the palette shell (F-630)', () => {
  it('names itself and opens on the trigger tab, with the six tabs in order', () => {
    renderPalette()
    expect(screen.getByRole('heading', { level: 2, name: 'Nodes' })).toBeInTheDocument()
    expect(screen.getByText('Drag nodes to the canvas')).toBeInTheDocument()
    expect(screen.getAllByRole('tab').map((t) => t.textContent)).toEqual(TABS.map(([t]) => t))
    expect(screen.getByRole('tab', { name: 'Trigger' })).toHaveAttribute('aria-selected', 'true')
  })
})

describe('every tab and entry (F-632..F-637)', () => {
  it.each(TABS)(
    'the %s tab lists its entries, each adding and dragging its type',
    async (tab, entries) => {
      const { onAdd, onDragStart } = renderPalette()
      await userEvent.setup().click(screen.getByRole('tab', { name: tab }))
      const panel = screen.getByRole('tabpanel')
      const buttons = within(panel).getAllByRole('button')
      expect(buttons.map((b) => b.getAttribute('aria-label'))).toEqual(
        entries.map(([, label]) => `Add ${label} node`)
      )
      entries.forEach(([type, label, description], i) => {
        expect(buttons[i]).toHaveTextContent(label)
        expect(buttons[i]).toHaveTextContent(description)
        expect(buttons[i]).toHaveAttribute('draggable', 'true')
        fireEvent.click(buttons[i])
        expect(onAdd).toHaveBeenLastCalledWith(type)
        fireEvent.dragStart(buttons[i])
        expect(onDragStart.mock.lastCall?.[1]).toBe(type)
      })
    }
  )
})

describe('the palette offers exactly the node types the app runs (F-697, X-261)', () => {
  const offered = TABS.flatMap(([, entries]) => entries.map(([type]) => type))

  it('offers each type once, 61 in all', () => {
    expect(new Set(offered).size).toBe(offered.length)
    expect(offered).toHaveLength(61)
  })

  it("equals the backend validator's node types", () => {
    expect([...offered].sort()).toEqual([...backendNodeTypes()].sort())
  })

  it('equals the types the canvas can draw', () => {
    expect([...offered].sort()).toEqual(Object.keys(nodeTypes).sort())
  })
})
