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
import {
  Activity,
  BarChart3,
  Bell,
  Briefcase,
  Calculator,
  Calendar,
  CalendarClock,
  CalendarRange,
  CalendarX,
  ClipboardList,
  Clock,
  FileSearch,
  FileText,
  Globe,
  Grid3X3,
  Group,
  History,
  Hourglass,
  Layers,
  Layers3,
  type LucideIcon,
  MessageCircle,
  Package,
  PackageCheck,
  Pencil,
  Radio,
  RadioTower,
  Receipt,
  Send,
  Shield,
  ShoppingCart,
  Sigma,
  SlidersHorizontal,
  Split,
  Square,
  Tag,
  Target,
  Timer,
  TrendingUp,
  Variable,
  Wallet,
  Webhook,
  WifiOff,
  XCircle,
  Zap,
} from 'lucide-react'
import { renderToStaticMarkup } from 'react-dom/server'
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

/**
 * Each entry's tile, as the web draws it (F-632..F-637): the icon, or for the
 * three gates a text tile (F-634), in its category colour, on a tint of it.
 */
const TILES: Array<[type: string, icon: LucideIcon | string, ink: string, tint: string]> = [
  ['start', Clock, 'text-orange-500', 'bg-orange-500/10'],
  ['priceAlert', Bell, 'text-orange-500', 'bg-orange-500/10'],
  ['webhookTrigger', Webhook, 'text-orange-500', 'bg-orange-500/10'],
  ['orderUpdateTrigger', PackageCheck, 'text-orange-500', 'bg-orange-500/10'],
  ['placeOrder', ShoppingCart, 'text-blue-500', 'bg-blue-500/10'],
  ['smartOrder', Zap, 'text-blue-500', 'bg-blue-500/10'],
  ['optionsOrder', TrendingUp, 'text-blue-500', 'bg-blue-500/10'],
  ['optionsMultiOrder', Layers, 'text-blue-500', 'bg-blue-500/10'],
  ['basketOrder', Package, 'text-blue-500', 'bg-blue-500/10'],
  ['splitOrder', Split, 'text-blue-500', 'bg-blue-500/10'],
  ['modifyOrder', Pencil, 'text-blue-500', 'bg-blue-500/10'],
  ['cancelOrder', XCircle, 'text-red-500', 'bg-red-500/10'],
  ['cancelAllOrders', XCircle, 'text-red-500', 'bg-red-500/10'],
  ['closePositions', Square, 'text-red-500', 'bg-red-500/10'],
  ['timeCondition', Clock, 'text-purple-500', 'bg-purple-500/10'],
  ['positionCheck', Briefcase, 'text-purple-500', 'bg-purple-500/10'],
  ['fundCheck', Wallet, 'text-purple-500', 'bg-purple-500/10'],
  ['priceCondition', TrendingUp, 'text-purple-500', 'bg-purple-500/10'],
  ['timeWindow', Clock, 'text-purple-500', 'bg-purple-500/10'],
  ['andGate', 'AND', 'text-purple-500', 'bg-purple-500/10'],
  ['orGate', 'OR', 'text-purple-500', 'bg-purple-500/10'],
  ['notGate', 'NOT', 'text-purple-500', 'bg-purple-500/10'],
  ['varCondition', SlidersHorizontal, 'text-purple-500', 'bg-purple-500/10'],
  ['getQuote', BarChart3, 'text-cyan-500', 'bg-cyan-500/10'],
  ['getDepth', Layers3, 'text-cyan-500', 'bg-cyan-500/10'],
  ['getOrderStatus', FileSearch, 'text-orange-500', 'bg-orange-500/10'],
  ['history', TrendingUp, 'text-cyan-500', 'bg-cyan-500/10'],
  ['indicator', Activity, 'text-cyan-500', 'bg-cyan-500/10'],
  ['strategyPnl', Wallet, 'text-cyan-500', 'bg-cyan-500/10'],
  ['priorPeriodOhlc', CalendarClock, 'text-cyan-500', 'bg-cyan-500/10'],
  ['barOffset', History, 'text-cyan-500', 'bg-cyan-500/10'],
  ['openPosition', Briefcase, 'text-violet-500', 'bg-violet-500/10'],
  ['expiry', Calendar, 'text-pink-500', 'bg-pink-500/10'],
  ['intervals', Clock, 'text-pink-500', 'bg-pink-500/10'],
  ['calendar', CalendarRange, 'text-purple-400', 'bg-purple-400/10'],
  ['multiQuotes', BarChart3, 'text-cyan-500', 'bg-cyan-500/10'],
  ['symbol', Tag, 'text-teal-500', 'bg-teal-500/10'],
  ['optionSymbol', Target, 'text-pink-500', 'bg-pink-500/10'],
  ['orderBook', ClipboardList, 'text-orange-500', 'bg-orange-500/10'],
  ['tradeBook', Receipt, 'text-orange-500', 'bg-orange-500/10'],
  ['positionBook', Briefcase, 'text-violet-500', 'bg-violet-500/10'],
  ['syntheticFuture', Calculator, 'text-pink-500', 'bg-pink-500/10'],
  ['optionChain', Grid3X3, 'text-pink-500', 'bg-pink-500/10'],
  ['holdings', Briefcase, 'text-amber-500', 'bg-amber-500/10'],
  ['funds', Wallet, 'text-amber-500', 'bg-amber-500/10'],
  ['margin', Shield, 'text-amber-500', 'bg-amber-500/10'],
  ['subscribeLtp', Radio, 'text-green-500', 'bg-green-500/10'],
  ['subscribeQuote', RadioTower, 'text-green-500', 'bg-green-500/10'],
  ['subscribeDepth', Layers3, 'text-green-500', 'bg-green-500/10'],
  ['unsubscribe', WifiOff, 'text-red-500', 'bg-red-500/10'],
  ['variable', Variable, 'text-purple-400', 'bg-purple-400/10'],
  ['mathExpression', Sigma, 'text-purple-500', 'bg-purple-500/10'],
  ['log', FileText, 'text-blue-400', 'bg-blue-400/10'],
  ['telegramAlert', Send, 'text-[#0088cc]', 'bg-[#0088cc]/10'],
  ['whatsappAlert', MessageCircle, 'text-[#25D366]', 'bg-[#25D366]/10'],
  ['delay', Timer, 'text-muted-foreground', 'bg-muted'],
  ['waitUntil', Hourglass, 'text-amber-500', 'bg-amber-500/10'],
  ['group', Group, 'text-muted-foreground', 'bg-muted'],
  ['httpRequest', Globe, 'text-primary', 'bg-primary/10'],
  ['holidays', CalendarX, 'text-purple-400', 'bg-purple-400/10'],
  ['timings', Clock, 'text-purple-400', 'bg-purple-400/10'],
]

/** The markup a tile holds for an entry: the icon, or the gate's word. */
function tileMarkup(icon: LucideIcon | string, ink: string): string {
  if (typeof icon === 'string') {
    return renderToStaticMarkup(<span className={`text-[9px] font-bold ${ink}`}>{icon}</span>)
  }
  const Icon = icon
  return renderToStaticMarkup(<Icon className={`h-3.5 w-3.5 ${ink}`} />)
}

describe("every entry's icon and colours (F-632..F-637)", () => {
  it('draws a tile for every palette entry, and only those', () => {
    expect(TILES.map(([type]) => type).sort()).toEqual(
      TABS.flatMap(([, entries]) => entries.map(([type]) => type)).sort()
    )
  })

  it.each(TABS)("the %s tab draws each entry's own icon and colours", async (tab, entries) => {
    renderPalette()
    await userEvent.setup().click(screen.getByRole('tab', { name: tab }))
    const buttons = within(screen.getByRole('tabpanel')).getAllByRole('button')
    entries.forEach(([type], i) => {
      const [, icon, ink, tint] = TILES.find(([t]) => t === type) ?? []
      if (icon === undefined || ink === undefined || tint === undefined) {
        throw new Error(`no tile for ${type}`)
      }
      const tile = buttons[i].firstElementChild?.firstElementChild as HTMLElement
      expect(tile.className.split(' '), type).toContain(tint)
      expect(tile.innerHTML, type).toBe(tileMarkup(icon, ink))
    })
  })

  it('writes AND, OR and NOT on the gate tiles in purple (F-634)', async () => {
    renderPalette()
    await userEvent.setup().click(screen.getByRole('tab', { name: 'Logic' }))
    for (const [name, text] of [
      ['AND Gate', 'AND'],
      ['OR Gate', 'OR'],
      ['NOT Gate', 'NOT'],
    ]) {
      const tile = screen.getByRole('button', { name: `Add ${name} node` }).firstElementChild
        ?.firstElementChild as HTMLElement
      expect(tile.textContent).toBe(text)
      expect(tile.querySelector('svg')).toBeNull()
      expect(tile.firstElementChild).toHaveClass('text-purple-500', 'font-bold')
      expect(tile).toHaveClass('bg-purple-500/10')
    }
  })
})
