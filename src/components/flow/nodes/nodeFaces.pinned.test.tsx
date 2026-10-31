/**
 * The trigger, order, condition and gate node faces, pinned as the web draws
 * them (parity F-674, F-675, F-676, F-677, F-678, F-679, F-680): titles,
 * subtitles, rows and their fallbacks, the branch captions, and the handles
 * xyflow renders (ids, target/source, position, horizontal offset). A wire
 * names a handle id, so a renamed or moved handle silently orphans every
 * saved workflow that used it. Each node is drawn on a real canvas.
 */

import { render } from '@testing-library/react'
import { type Node, ReactFlow, ReactFlowProvider } from '@xyflow/react'
import { describe, expect, it } from 'vitest'
import { nodeTypes } from './index'

function face(type: string, data: Record<string, unknown> = {}): HTMLElement {
  const node: Node = { id: `n-${type}`, type, position: { x: 0, y: 0 }, data }
  const { container } = render(
    <div style={{ width: 800, height: 600 }}>
      <ReactFlowProvider>
        <ReactFlow nodes={[node]} edges={[]} nodeTypes={nodeTypes} />
      </ReactFlowProvider>
    </div>
  )
  const el = container.querySelector<HTMLElement>(`.react-flow__node[data-id="${node.id}"]`)
  if (!el) throw new Error(`${type} not drawn`)
  return el
}

interface HandleFace {
  id: string | null
  kind: 'target' | 'source'
  position: string | undefined
  left: string
}

/** Every handle xyflow drew for the node, in DOM order. */
function handles(el: HTMLElement): HandleFace[] {
  return [...el.querySelectorAll<HTMLElement>('.react-flow__handle')].map((h) => ({
    id: h.dataset.handleid ?? null,
    kind: h.classList.contains('target') ? 'target' : 'source',
    position: h.dataset.handlepos,
    left: h.style.left,
  }))
}

/** The innermost element (of `tag`, when given) whose whole text is `text`. */
function leaf(el: HTMLElement, text: string, tag = '*'): HTMLElement {
  const found = [...el.querySelectorAll<HTMLElement>(tag)].find(
    (child) => child.children.length === 0 && child.textContent === text
  )
  if (!found) throw new Error(`no "${text}" in ${el.textContent}`)
  return found
}

/** The subtitle drawn under a node's title. */
function subtitle(el: HTMLElement, title: string): string | null | undefined {
  return leaf(el, title, 'div').nextElementSibling?.textContent
}

/** The value drawn beside a row's label. */
function row(el: HTMLElement, label: string): string | null | undefined {
  return leaf(el, label, 'span').nextElementSibling?.textContent
}

function has(el: HTMLElement, text: string): boolean {
  return [...el.querySelectorAll<HTMLElement>('*')].some(
    (child) => child.children.length === 0 && child.textContent === text
  )
}

const ONE_IN_ONE_OUT: HandleFace[] = [
  { id: null, kind: 'target', position: 'top', left: '' },
  { id: null, kind: 'source', position: 'bottom', left: '' },
]

const TRUE_FALSE: HandleFace[] = [
  { id: null, kind: 'target', position: 'top', left: '' },
  { id: 'true', kind: 'source', position: 'bottom', left: '25%' },
  { id: 'false', kind: 'source', position: 'bottom', left: '75%' },
]

/** The branch captions under the node, left then right, with their colours. */
function captions(el: HTMLElement): Array<[string | null, string]> {
  const row = [...el.querySelectorAll<HTMLElement>('div.mt-2')].at(-1)
  return [...(row?.children ?? [])].map((c) => [c.textContent, c.className])
}

describe('F-674 the Start node', () => {
  it('takes no wire in and gives one unnamed wire out', () => {
    expect(handles(face('start', { scheduleType: 'daily' }))).toEqual([
      { id: null, kind: 'source', position: 'bottom', left: '' },
    ])
  })

  it.each([
    ['once', 'One-time', 'lucide-calendar'],
    ['daily', 'Daily', 'lucide-clock'],
    ['weekly', 'Weekly', 'lucide-calendar-days'],
    ['interval', 'Interval', 'lucide-timer'],
    ['cron', 'Schedule', 'lucide-clock'],
  ])('names a %s schedule "%s" with its own icon', (scheduleType, shown, icon) => {
    const el = face('start', { scheduleType })
    expect(subtitle(el, 'Start')).toBe(shown)
    expect(el.querySelector('svg')?.classList.contains(icon)).toBe(true)
  })

  it.each([
    [{ intervalValue: 30, intervalUnit: 'seconds' }, '30s'],
    [{ intervalValue: 5, intervalUnit: 'minutes' }, '5m'],
    [{ intervalValue: 2, intervalUnit: 'hours' }, '2h'],
    [{}, '1m'],
  ])('shows an interval schedule as Every: %o -> %s', (interval, shown) => {
    const el = face('start', { scheduleType: 'interval', ...interval })
    expect(row(el, 'Every:')).toBe(shown)
    expect(has(el, 'Time:')).toBe(false)
  })

  it('shows the time of any other schedule, 09:15 when unset', () => {
    expect(row(face('start', { scheduleType: 'once', time: '10:45' }), 'Time:')).toBe('10:45')
    expect(row(face('start', { scheduleType: 'daily' }), 'Time:')).toBe('09:15')
  })

  // Desktop (web defect): the web showed "Schedule" and Time: 09:15, but no
  // type or "manual" is never scheduled; the face now matches the panel.
  it.each([
    ['no type', {}],
    ['manual', { scheduleType: 'manual', time: '10:45', days: [0, 2] }],
  ])('says there is no schedule for %s, with no time or days', (_case, data) => {
    const el = face('start', data)
    expect(subtitle(el, 'Start')).toBe('No schedule')
    expect(has(el, 'Run it with Run Now')).toBe(true)
    expect(has(el, 'Time:')).toBe(false)
    expect(has(el, 'Every:')).toBe(false)
    expect(has(el, 'Days:')).toBe(false)
  })

  it('spells the chosen days as M T W T F S S letters only for a partial week', () => {
    expect(row(face('start', { scheduleType: 'daily', days: [0, 2, 4] }), 'Days:')).toBe('MWF')
    expect(row(face('start', { scheduleType: 'weekly', days: [5, 6] }), 'Days:')).toBe('SS')
    const everyDay = face('start', { scheduleType: 'daily', days: [0, 1, 2, 3, 4, 5, 6] })
    expect(has(everyDay, 'Days:')).toBe(false)
    expect(has(face('start', { scheduleType: 'daily', days: [] }), 'Days:')).toBe(false)
    expect(has(face('start', { scheduleType: 'once', days: [0] }), 'Days:')).toBe(false)
  })
})

describe('F-675 the Price Alert node', () => {
  it('takes a wire in at the top and branches true left, false right', () => {
    expect(handles(face('priceAlert'))).toEqual(TRUE_FALSE)
  })

  it('captions the branches Yes and No in the buy and sell colours', () => {
    expect(captions(face('priceAlert'))).toEqual([
      ['Yes', 'text-buy'],
      ['No', 'text-sell'],
    ])
  })

  it('titles itself with the exchange, NSE when unset, and shows the symbol or -', () => {
    const el = face('priceAlert', { exchange: 'NFO', symbol: 'NIFTY25AUGFUT' })
    expect(subtitle(el, 'Price Alert')).toBe('NFO')
    expect(row(el, 'Symbol')).toBe('NIFTY25AUGFUT')
    const blank = face('priceAlert')
    expect(subtitle(blank, 'Price Alert')).toBe('NSE')
    expect(row(blank, 'Symbol')).toBe('-')
  })

  it.each([
    ['above', 'LTP Above', 'LTP > 812.5'],
    ['below', 'LTP Below', 'LTP < 812.5'],
    ['crosses_above', 'Crosses Above', 'LTP crosses > 812.5'],
    ['crosses_below', 'Crosses Below', 'LTP crosses < 812.5'],
  ])('reads a %s alert as "%s" with the footer "%s"', (condition, label, footer) => {
    const el = face('priceAlert', { condition, price: 812.5 })
    expect(row(el, label)).toBe('812.5')
    expect(has(el, footer)).toBe(true)
  })

  it('falls back to Condition, > and 0 when nothing is set', () => {
    const el = face('priceAlert', { condition: '' })
    expect(row(el, 'Condition')).toBe('0')
    expect(has(el, 'LTP > 0')).toBe(true)
  })

  it('shows a live LTP row only when the node carries one', () => {
    expect(row(face('priceAlert', { ltp: 812.456 }), 'LTP')).toBe('812.46')
    expect(has(face('priceAlert'), 'LTP')).toBe(false)
  })
})

describe('F-676 the Webhook and Order Update triggers', () => {
  it('take no wire in and give one unnamed wire out', () => {
    for (const type of ['webhookTrigger', 'orderUpdateTrigger']) {
      expect(handles(face(type)), type).toEqual([
        { id: null, kind: 'source', position: 'bottom', left: '' },
      ])
    }
  })

  it('a webhook reads "Webhook / External Trigger" and shows its label only when set', () => {
    const el = face('webhookTrigger', { label: 'TradingView alert' })
    expect(subtitle(el, 'Webhook')).toBe('External Trigger')
    expect(has(el, 'TradingView alert')).toBe(true)
    expect(face('webhookTrigger').querySelector('.bg-muted\\/50')).toBeNull()
  })

  it('an order update shows the status it waits for, complete by default', () => {
    expect(subtitle(face('orderUpdateTrigger', { status: 'rejected' }), 'Order Update')).toBe(
      'rejected'
    )
    expect(subtitle(face('orderUpdateTrigger'), 'Order Update')).toBe('complete')
  })

  it('an order update watches the order id, else the symbol, else any order', () => {
    expect(has(face('orderUpdateTrigger', { orderId: '2511', symbol: 'SBIN' }), '2511')).toBe(true)
    expect(has(face('orderUpdateTrigger', { symbol: 'SBIN' }), 'SBIN')).toBe(true)
    expect(has(face('orderUpdateTrigger'), 'Any order')).toBe(true)
  })
})

describe('F-677 the order nodes', () => {
  it.each([
    'placeOrder',
    'smartOrder',
    'optionsOrder',
    'optionsMultiOrder',
    'basketOrder',
    'splitOrder',
    'modifyOrder',
    'cancelOrder',
    'cancelAllOrders',
    'closePositions',
  ])('a %s takes one wire in and gives one unnamed wire out', (type) => {
    expect(handles(face(type))).toEqual(ONE_IN_ONE_OUT)
  })

  it('Place Order: exchange, symbol, side badge, quantity, price type and product', () => {
    const el = face('placeOrder', {
      exchange: 'NFO',
      symbol: 'NIFTY25AUGFUT',
      action: 'SELL',
      quantity: 75,
      priceType: 'LIMIT',
    })
    expect(subtitle(el, 'Place Order')).toBe('NFO')
    expect(row(el, 'Symbol')).toBe('NIFTY25AUGFUT')
    expect(leaf(el, 'SELL').classList.contains('badge-sell')).toBe(true)
    expect(row(el, 'Qty:')).toBe('75')
    expect(has(el, 'LIMIT')).toBe(true)
    // An NFO order with no product of its own goes out NRML.
    expect(has(el, 'NRML')).toBe(true)
  })

  it('Place Order: the fallbacks NSE, -, BUY, 0, MARKET and the cash product', () => {
    const el = face('placeOrder')
    expect(subtitle(el, 'Place Order')).toBe('NSE')
    expect(row(el, 'Symbol')).toBe('-')
    expect(has(el, 'BUY')).toBe(true)
    expect(row(el, 'Qty:')).toBe('0')
    expect(has(el, 'MARKET')).toBe(true)
    expect(has(el, 'MIS')).toBe(true)
    expect(leaf(face('placeOrder', { action: 'BUY' }), 'BUY').classList.contains('badge-buy')).toBe(
      true
    )
    expect(has(face('placeOrder', { exchange: 'NFO', product: 'MIS' }), 'MIS')).toBe(true)
  })

  it('Place Order shows a live LTP row only when the node carries one', () => {
    expect(row(face('placeOrder', { ltp: 101.5 }), 'LTP')).toBe('101.50')
    expect(has(face('placeOrder'), 'LTP')).toBe(false)
  })

  it('Smart Order adds the target position, 0 when unset', () => {
    const el = face('smartOrder', {
      exchange: 'BSE',
      action: 'SELL',
      quantity: 5,
      positionSize: -10,
    })
    expect(subtitle(el, 'Smart Order')).toBe('BSE')
    expect(leaf(el, 'SELL').classList.contains('badge-sell')).toBe(true)
    expect(row(el, 'Qty:')).toBe('5')
    expect(row(el, 'Target Pos')).toBe('-10')
    expect(row(face('smartOrder'), 'Target Pos')).toBe('0')
    expect(has(face('smartOrder', { exchange: 'NFO' }), 'NRML')).toBe(true)
  })

  it('Options: underlying, strike, expiry, side, quantity, product and price type', () => {
    const el = face('optionsOrder', {
      underlying: 'BANKNIFTY',
      offset: 'OTM2',
      optionType: 'PE',
      expiryType: 'next_month',
      action: 'SELL',
      quantity: 30,
    })
    expect(subtitle(el, 'Options')).toBe('BANKNIFTY')
    expect(row(el, 'Strike')).toBe('OTM2 PE')
    expect(row(el, 'Expiry')).toBe('Next Month')
    expect(leaf(el, 'SELL').classList.contains('badge-sell')).toBe(true)
    expect(row(el, 'Qty:')).toBe('30')
    const blank = face('optionsOrder')
    expect(subtitle(blank, 'Options')).toBe('NIFTY')
    expect(row(blank, 'Strike')).toBe('ATM CE')
    expect(row(blank, 'Expiry')).toBe('This Week')
    // An option trades NRML whatever its underlying's exchange reads.
    expect(has(blank, 'NRML')).toBe(true)
    expect(has(blank, 'MARKET')).toBe(true)
  })

  it.each([
    ['placeOrder', 'badge-buy', 'badge-sell'],
    ['smartOrder', 'badge-buy', 'badge-sell'],
    ['optionsOrder', 'badge-buy', 'badge-sell'],
    ['splitOrder', 'text-green-500', 'text-red-500'],
  ])(
    '%s with no stored side shows BUY in the buy colour (desktop: the web drew it red)',
    (type, buy, sell) => {
      const side = leaf(face(type), 'BUY')
      expect(side.classList.contains(buy)).toBe(true)
      expect(side.classList.contains(sell)).toBe(false)
      expect(leaf(face(type, { action: 'SELL' }), 'SELL').classList.contains(sell)).toBe(true)
    }
  )

  it('Multi-Leg: strategy, leg count, expiry and the side, SELL by default', () => {
    const el = face('optionsMultiOrder', {
      underlying: 'FINNIFTY',
      strategy: 'iron_condor',
      expiryType: 'next_week',
      action: 'BUY',
    })
    expect(subtitle(el, 'Multi-Leg')).toBe('FINNIFTY')
    expect(row(el, 'Strategy')).toBe('Iron Condor')
    expect(row(el, 'Legs')).toBe('4')
    expect(row(el, 'Expiry')).toBe('Next Week')
    expect(leaf(el, 'BUY').classList.contains('text-buy')).toBe(true)

    const blank = face('optionsMultiOrder', { strategy: 'straddle' })
    expect(subtitle(blank, 'Multi-Leg')).toBe('NIFTY')
    expect(row(blank, 'Legs')).toBe('2')
    expect(row(blank, 'Expiry')).toBe('This Week')
    expect(leaf(blank, 'SELL').classList.contains('text-sell')).toBe(true)
    expect(has(blank, 'NRML')).toBe(true)
  })

  it('Multi-Leg counts the custom legs it holds over the strategy default', () => {
    const custom = face('optionsMultiOrder', { strategy: 'custom', legs: [{}, {}, {}] })
    expect(row(custom, 'Strategy')).toBe('Custom')
    expect(row(custom, 'Legs')).toBe('3')
    expect(row(face('optionsMultiOrder', { strategy: 'custom' }), 'Legs')).toBe('0')
  })

  it('Basket Order counts CSV rows or array entries and names the basket', () => {
    const csv = face('basketOrder', {
      orders: 'SBIN,NSE,BUY,1\n\nINFY,NSE,SELL,2\n',
      basketName: 'Hedge',
    })
    expect(subtitle(csv, 'Basket Order')).toBe('Multi-order')
    expect(row(csv, 'Orders')).toBe('2')
    expect(has(csv, 'Hedge')).toBe(true)
    expect(row(face('basketOrder', { orders: [{}, {}, {}] }), 'Orders')).toBe('3')
    const blank = face('basketOrder')
    expect(row(blank, 'Orders')).toBe('0')
    expect(has(blank, 'Batch execution')).toBe(true)
  })

  it('Split Order: exchange and the side in the buy or sell colour', () => {
    const el = face('splitOrder', { exchange: 'NFO', symbol: 'SBIN', action: 'SELL' })
    expect(subtitle(el, 'Split Order')).toBe('NFO')
    expect(leaf(el, 'SELL').classList.contains('text-red-500')).toBe(true)
    expect(
      leaf(face('splitOrder', { action: 'BUY' }), 'BUY').classList.contains('text-green-500')
    ).toBe(true)
    expect(subtitle(face('splitOrder'), 'Split Order')).toBe('NSE')
  })

  it('Modify Order: the last six of the order id, else Variable, and only the changes set', () => {
    const el = face('modifyOrder', { orderId: '250829000123456', newPrice: 101.5, newQuantity: 20 })
    expect(subtitle(el, 'Modify Order')).toBe('Edit order')
    expect(row(el, 'Order ID')).toBe('...123456')
    expect(row(el, 'New Price')).toBe('101.5')
    expect(row(el, 'New Qty')).toBe('20')
    const blank = face('modifyOrder')
    expect(row(blank, 'Order ID')).toBe('Variable')
    expect(has(blank, 'New Price')).toBe(false)
    expect(has(blank, 'New Qty')).toBe(false)
  })

  it('Cancel Order: By Order ID, the last six of the id, else Variable', () => {
    const el = face('cancelOrder', { orderId: '250829000654321' })
    expect(subtitle(el, 'Cancel Order')).toBe('By Order ID')
    expect(row(el, 'Order ID')).toBe('...654321')
    expect(row(face('cancelOrder'), 'Order ID')).toBe('Variable')
  })

  it('Cancel All reads "Cancel All / Orders" and says what it does', () => {
    const el = face('cancelAllOrders')
    expect(subtitle(el, 'Cancel All')).toBe('Orders')
    expect(has(el, 'Cancels all open orders')).toBe(true)
  })

  it('Close Positions reads Close All until a symbol scopes it, even with an exchange set', () => {
    const all = face('closePositions', { exchange: 'NSE' })
    expect(subtitle(all, 'Close All')).toBe('Positions')
    expect(has(all, 'Squares off all positions')).toBe(true)
    expect(has(all, 'Symbol')).toBe(false)

    const scoped = face('closePositions', { symbol: 'SBIN', exchange: 'NSE', product: 'MIS' })
    expect(subtitle(scoped, 'Close')).toBe('Positions')
    expect(row(scoped, 'Symbol')).toBe('SBIN')
    expect(row(scoped, 'Exchange')).toBe('NSE')
    expect(row(scoped, 'Product')).toBe('MIS')
    expect(has(scoped, 'Squares off all positions')).toBe(false)
    expect(has(face('closePositions', { symbol: 'SBIN' }), 'Exchange')).toBe(false)
  })
})

describe('F-677 the order rows the pins above skip', () => {
  /** The texts of the row holding `text`, left to right. */
  function rowOf(el: HTMLElement, text: string): Array<string | null> {
    return [...(leaf(el, text).parentElement?.children ?? [])].map((c) => c.textContent)
  }

  it('Smart Order shows the symbol or -, then the price type and the product', () => {
    const el = face('smartOrder', { symbol: 'SBIN', priceType: 'SL-M', product: 'CNC' })
    expect(row(el, 'Symbol')).toBe('SBIN')
    expect(rowOf(el, 'SL-M')).toEqual(['SL-M', 'CNC'])
    const blank = face('smartOrder')
    expect(row(blank, 'Symbol')).toBe('-')
    expect(rowOf(blank, 'MARKET')).toEqual(['MARKET', 'MIS'])
  })

  it('Split Order shows the symbol or -', () => {
    expect(row(face('splitOrder', { symbol: 'RELIANCE' }), 'Symbol')).toBe('RELIANCE')
    expect(row(face('splitOrder'), 'Symbol')).toBe('-')
  })

  it('Options shows a stored product and price type, product first', () => {
    const el = face('optionsOrder', { product: 'MIS', priceType: 'LIMIT' })
    expect(rowOf(el, 'LIMIT')).toEqual(['MIS', 'LIMIT'])
  })
})

describe('F-678 the condition nodes', () => {
  it.each(['positionCheck', 'fundCheck', 'priceCondition', 'timeWindow', 'varCondition'])(
    'a %s branches true left, false right, captioned True and False',
    (type) => {
      const el = face(type)
      expect(handles(el)).toEqual(TRUE_FALSE)
      expect(captions(el)).toEqual([
        ['True', 'text-buy'],
        ['False', 'text-sell'],
      ])
    }
  )

  it.each([
    ['exists', 'Has Position', false],
    ['not_exists', 'No Position', false],
    ['quantity_above', 'Qty Above', true],
    ['quantity_below', 'Qty Below', true],
    ['pnl_above', 'P&L Above', true],
    ['pnl_below', 'P&L Below', true],
  ])('Position Check reads %s as "%s"', (condition, label, threshold) => {
    const el = face('positionCheck', { symbol: 'SBIN', condition, threshold: 25 })
    expect(subtitle(el, 'Position')).toBe('Check')
    expect(row(el, 'Symbol')).toBe('SBIN')
    expect(row(el, 'Check')).toBe(label)
    expect(has(el, 'Threshold:')).toBe(threshold)
    if (threshold) expect(row(el, 'Threshold:')).toBe('25')
  })

  it('Position Check shows threshold 0 when unset and an unknown check as stored', () => {
    expect(row(face('positionCheck', { condition: 'pnl_above' }), 'Threshold:')).toBe('0')
    expect(row(face('positionCheck', { condition: 'odd_check' }), 'Check')).toBe('odd_check')
    expect(row(face('positionCheck'), 'Symbol')).toBe('-')
  })

  it('Fund Check: the minimum, 0 when unset, and what it checks', () => {
    const el = face('fundCheck', { minAvailable: 500 })
    expect(subtitle(el, 'Fund')).toBe('Check')
    expect(row(el, 'Min')).toBe('500')
    expect(has(el, 'Checks available margin')).toBe(true)
    expect(row(face('fundCheck'), 'Min')).toBe('0')
  })

  it.each([
    [{ field: 'ltp', operator: '>', value: 100 }, 'LTP > 100'],
    [{ field: 'open', operator: '<', value: 5 }, 'Open < 5'],
    [{ field: 'high', operator: '==', value: 7 }, 'High = 7'],
    [{ field: 'low', operator: '>=', value: 8 }, 'Low >= 8'],
    [{ field: 'prev_close', operator: '<=', value: 9 }, 'Prev Close <= 9'],
    [{ field: 'change_percent', operator: '!=', value: 1 }, 'Change % != 1'],
    [{}, 'LTP > 0'],
  ])('Price Condition reads %o as "%s"', (check, shown) => {
    const el = face('priceCondition', { symbol: 'SBIN', ...check })
    expect(subtitle(el, 'Price')).toBe('Condition')
    expect(row(el, 'Symbol')).toBe('SBIN')
    expect(row(el, 'Check')).toBe(shown)
  })

  it('Time Window: start and end, 09:15-15:30 by default, Outside window when inverted', () => {
    const el = face('timeWindow', { startTime: '10:00', endTime: '14:00', invertCondition: true })
    expect(subtitle(el, 'Time')).toBe('Window')
    expect(row(el, 'Start')).toBe('10:00')
    expect(row(el, 'End')).toBe('14:00')
    expect(has(el, 'Outside window')).toBe(true)
    const blank = face('timeWindow')
    expect(row(blank, 'Start')).toBe('09:15')
    expect(row(blank, 'End')).toBe('15:30')
    expect(has(blank, 'Outside window')).toBe(false)
  })

  it('Var Condition: left, operator and right, with the whole text as its title', () => {
    const el = face('varCondition', {
      leftValue: '{{ltp}}',
      operator: '==',
      rightValue: '{{entry}}',
    })
    expect(subtitle(el, 'Var')).toBe('Condition')
    const shown = leaf(el, '{{ltp}} = {{entry}}')
    expect(shown.title).toBe('{{ltp}} = {{entry}}')
    expect(has(face('varCondition'), 'left > right')).toBe(true)
  })
})

describe('F-679 the Time Condition and NOT gate', () => {
  it('Time Condition branches yes left, no right, captioned Yes and No', () => {
    const el = face('timeCondition')
    expect(handles(el)).toEqual([
      { id: null, kind: 'target', position: 'top', left: '' },
      { id: 'yes', kind: 'source', position: 'bottom', left: '25%' },
      { id: 'no', kind: 'source', position: 'bottom', left: '75%' },
    ])
    expect(captions(el)).toEqual([
      ['Yes', 'text-buy'],
      ['No', 'text-sell'],
    ])
  })

  it.each([
    ['entry', 'Entry'],
    ['exit', 'Exit'],
    ['custom', 'Condition'],
  ])('Time Condition names a %s check "%s"', (conditionType, shown) => {
    expect(subtitle(face('timeCondition', { conditionType }), 'Time')).toBe(shown)
  })

  it.each([
    ['==', '= 15:00'],
    ['>=', '>= 15:00'],
    ['<=', '<= 15:00'],
    ['>', '> 15:00'],
    ['<', '< 15:00'],
  ])('Time Condition shows the %s check as "%s"', (operator, shown) => {
    const el = face('timeCondition', { operator, targetTime: '15:00', label: 'Square off' })
    expect(row(el, 'Time')).toBe(shown)
    expect(has(el, 'Square off')).toBe(true)
  })

  it('Time Condition falls back to = 09:30 with no label', () => {
    const el = face('timeCondition')
    expect(row(el, 'Time')).toBe('= 09:30')
    expect(el.querySelector('.truncate')).toBeNull()
  })

  it('NOT takes its wire on the "input" handle and branches yes and no', () => {
    const el = face('notGate')
    expect(handles(el)).toEqual([
      { id: 'input', kind: 'target', position: 'top', left: '' },
      { id: 'yes', kind: 'source', position: 'bottom', left: '25%' },
      { id: 'no', kind: 'source', position: 'bottom', left: '75%' },
    ])
    expect(has(el, 'NOT')).toBe(true)
    expect(has(el, 'Inverts condition')).toBe(true)
    expect(captions(el)).toEqual([
      ['Yes', 'text-buy'],
      ['No', 'text-sell'],
    ])
  })
})

describe('F-680 the AND and OR gates', () => {
  it.each([
    ['andGate', 'AND', 'All conditions must be true'],
    ['orGate', 'OR', 'Any condition can be true'],
  ])('%s reads %s and "%s", captioned True and False', (type, tile, caption) => {
    const el = face(type)
    expect(has(el, tile)).toBe(true)
    expect(has(el, caption)).toBe(true)
    expect(captions(el)).toEqual([
      ['True', 'text-buy'],
      ['False', 'text-sell'],
    ])
  })

  it.each([
    ['andGate', undefined, 2],
    ['orGate', undefined, 2],
    ['andGate', 3, 3],
    ['orGate', 5, 5],
  ])(
    '%s with inputCount %s draws %i inputs spaced evenly, then true and false',
    (type, inputCount, n) => {
      const drawn = handles(face(type, inputCount === undefined ? {} : { inputCount }))
      const inputs = drawn.filter((h) => h.kind === 'target')
      expect(inputs.map((h) => h.id)).toEqual(Array.from({ length: n }, (_, i) => `input-${i}`))
      inputs.forEach((h, i) => {
        expect(h.position).toBe('top')
        expect(Number.parseFloat(h.left)).toBeCloseTo(((i + 1) / (n + 1)) * 100, 5)
      })
      expect(drawn.filter((h) => h.kind === 'source')).toEqual([
        { id: 'true', kind: 'source', position: 'bottom', left: '25%' },
        { id: 'false', kind: 'source', position: 'bottom', left: '75%' },
      ])
    }
  )
})
