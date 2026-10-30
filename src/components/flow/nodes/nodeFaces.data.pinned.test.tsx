/**
 * The data and streaming node faces, pinned as the web draws them (parity
 * F-681, IND-085, F-682): titles, subtitles, rows and their fallbacks, the
 * output variable, and one target plus one unnamed source handle on each.
 * Each node is drawn on a real canvas.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
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

function handles(el: HTMLElement) {
  return [...el.querySelectorAll<HTMLElement>('.react-flow__handle')].map((h) => ({
    id: h.dataset.handleid ?? null,
    kind: h.classList.contains('target') ? 'target' : 'source',
    position: h.dataset.handlepos,
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

const DATA_TYPES = [
  'getQuote',
  'getDepth',
  'getOrderStatus',
  'openPosition',
  'history',
  'indicator',
  'priorPeriodOhlc',
  'strategyPnl',
  'barOffset',
  'expiry',
  'intervals',
  'multiQuotes',
  'symbol',
  'optionSymbol',
  'orderBook',
  'tradeBook',
  'positionBook',
  'syntheticFuture',
  'optionChain',
  'holdings',
  'funds',
  'margin',
  'holidays',
  'timings',
  'calendar',
]

const STREAM_TYPES = ['subscribeLtp', 'subscribeQuote', 'subscribeDepth', 'unsubscribe']

describe('F-681 the data nodes', () => {
  it.each([...DATA_TYPES, ...STREAM_TYPES])(
    'a %s takes one wire in and gives one unnamed wire out',
    (type) => {
      expect(handles(face(type))).toEqual([
        { id: null, kind: 'target', position: 'top' },
        { id: null, kind: 'source', position: 'bottom' },
      ])
    }
  )

  it.each([
    ['getQuote', 'Get Quote'],
    ['getDepth', 'Depth'],
    ['openPosition', 'Position'],
    ['history', 'History'],
    ['priorPeriodOhlc', 'Prev Day'],
    ['multiQuotes', 'Multi Quotes'],
    ['symbol', 'Symbol Info'],
  ])('%s shows its exchange under "%s", NSE when unset', (type, title) => {
    expect(subtitle(face(type, { exchange: 'BSE' }), title)).toBe('BSE')
    expect(subtitle(face(type), title)).toBe('NSE')
  })

  it.each([
    ['getQuote', 'Get Quote'],
    ['getDepth', 'Depth'],
    ['openPosition', 'Position'],
    ['history', 'History'],
    ['priorPeriodOhlc', 'Prev Day'],
    ['symbol', 'Symbol Info'],
    ['barOffset', 'Bar Offset'],
  ])('%s shows the symbol, - when unset', (type) => {
    expect(row(face(type, { symbol: 'RELIANCE' }), 'Symbol')).toBe('RELIANCE')
    expect(row(face(type), 'Symbol')).toBe('-')
  })

  it.each([
    ['getQuote', { outputVariable: 'quote' }, 'quote'],
    ['priorPeriodOhlc', { outputVariable: 'pdh' }, 'pdh'],
    ['strategyPnl', { outputVariable: 'pnl' }, 'pnl'],
    ['barOffset', { outputVariable: 'bar' }, 'bar'],
    ['expiry', { outputVariable: 'exp' }, 'exp'],
    ['intervals', { outputVariable: 'tf' }, 'tf'],
    ['symbol', { outputVariable: 'info' }, 'info'],
    ['optionSymbol', { outputVariable: 'opt' }, 'opt'],
    ['orderBook', { outputVariable: 'orders' }, 'orders'],
    ['tradeBook', { outputVariable: 'trades' }, 'trades'],
    ['positionBook', { outputVariable: 'book' }, 'book'],
    ['syntheticFuture', { outputVariable: 'syn' }, 'syn'],
    ['optionChain', { outputVariable: 'chain' }, 'chain'],
    ['holidays', { outputVariable: 'hol' }, 'hol'],
    ['timings', { outputVariable: 'hours' }, 'hours'],
    ['calendar', { outputVariable: 'cal' }, 'cal'],
  ])('%s shows its output variable %o only when set', (type, data, shown) => {
    expect(has(face(type, data), shown)).toBe(true)
    expect(has(face(type), shown)).toBe(false)
  })

  it('Depth says it reads 5-level bid/ask', () => {
    expect(has(face('getDepth'), '5-level bid/ask')).toBe(true)
  })

  it('Order Status shows the last six of the order id, - when unset', () => {
    const el = face('getOrderStatus', { orderId: '250829000777111' })
    expect(subtitle(el, 'Order Status')).toBe('Check order')
    expect(row(el, 'Order ID')).toBe('...777111')
    expect(row(face('getOrderStatus'), 'Order ID')).toBe('-')
  })

  it('Open Position shows the product, else the exchange default', () => {
    expect(has(face('openPosition', { exchange: 'NFO' }), 'NRML')).toBe(true)
    expect(has(face('openPosition', { exchange: 'NSE' }), 'MIS')).toBe(true)
    expect(has(face('openPosition', { exchange: 'NFO', product: 'MIS' }), 'MIS')).toBe(true)
  })

  it('History shows the interval label or the stored interval, and the days', () => {
    expect(has(face('history', { interval: '5m', days: 10 }), '5 Min')).toBe(true)
    expect(has(face('history', { interval: '5m', days: 10 }), '10 days')).toBe(true)
    expect(has(face('history', { interval: '1h', days: 5 }), '1 Hour')).toBe(true)
    expect(has(face('history', { interval: '3h', days: 5 }), '3 Hours')).toBe(true)
    expect(has(face('history', { interval: '7m', days: 5 }), '7m')).toBe(true)
  })

  // Decision 32 (the web showed 1d and 30 days): what the run uses.
  it('History with nothing stored shows the run defaults: 5 Min, and that days are required', () => {
    const history = readFileSync(join(process.cwd(), 'src-tauri/src/flow/nodes/history.rs'), 'utf8')
    expect(history).toMatch(/get_str\(data, "interval", "5m"\)/)
    expect(history).toMatch(/try_get_int\(data, "days", 0\)/)
    const blank = face('history')
    expect(has(blank, '5 Min')).toBe(true)
    expect(has(blank, 'Days required')).toBe(true)
    expect(has(blank, '1d')).toBe(false)
    expect(has(blank, '30 days')).toBe(false)
  })

  it('History shows Daily for D and for a stored 1d, which the run sends as D', () => {
    expect(has(face('history', { interval: 'D', days: 5 }), 'Daily')).toBe(true)
    expect(has(face('history', { interval: '1d', days: 5 }), 'Daily')).toBe(true)
  })

  it.each([
    [{ days: 0 }, 'Days required'],
    [{ days: -3 }, 'Days required'],
    [{ days: '' }, 'Days required'],
    [{ days: '45' }, '45 days'],
    [{ days: '{{lookback}}' }, '{{lookback}} days'],
    [{ startDate: '2026-10-01', endDate: '2026-10-30', days: 5 }, '2026-10-01 to 2026-10-30'],
    [{ startDate: '2026-10-01' }, 'Days required'],
  ])('History with %j shows the range as "%s"', (data, shown) => {
    expect(has(face('history', { interval: '5m', ...data }), shown)).toBe(true)
  })

  it.each([
    ['previous_hour', 'Prev Hour'],
    ['previous_day', 'Prev Day'],
    ['previous_week', 'Prev Week'],
    ['previous_month', 'Prev Month'],
    ['', 'Prev Day'],
  ])('Prior Period OHLC titles a %s period "%s"', (period, title) => {
    expect(subtitle(face('priorPeriodOhlc', { period }), title)).toBe('NSE')
  })

  it('Strategy P&L names the strategy, this workflow by default', () => {
    const el = face('strategyPnl', { strategy: 'Straddle 920' })
    expect(subtitle(el, 'Strategy P&L')).toBe('realized + unrealized')
    expect(row(el, 'Strategy')).toBe('Straddle 920')
    expect(row(face('strategyPnl'), 'Strategy')).toBe('this workflow')
  })

  it('Bar Offset shows the interval (D by default) and the bars back (0 by default)', () => {
    const el = face('barOffset', { interval: '15m', offsetBars: 3 })
    expect(subtitle(el, 'Bar Offset')).toBe('15m')
    expect(row(el, 'Bars back')).toBe('3')
    const blank = face('barOffset')
    expect(subtitle(blank, 'Bar Offset')).toBe('D')
    expect(row(blank, 'Bars back')).toBe('0')
  })

  it('Expiry shows exchange, symbol and instrument type, NFO / NIFTY / options by default', () => {
    const el = face('expiry', { exchange: 'BFO', symbol: 'SENSEX', instrumenttype: 'futures' })
    expect(subtitle(el, 'Expiry')).toBe('BFO')
    expect(row(el, 'Symbol')).toBe('SENSEX')
    expect(row(el, 'Type')).toBe('futures')
    const blank = face('expiry')
    expect(subtitle(blank, 'Expiry')).toBe('NFO')
    expect(row(blank, 'Symbol')).toBe('NIFTY')
    const type = leaf(blank, 'options', 'span')
    expect(type.classList.contains('uppercase')).toBe(true)
  })

  it('Intervals reads Time frames and what it fetches', () => {
    const el = face('intervals')
    expect(subtitle(el, 'Intervals')).toBe('Time frames')
    expect(has(el, 'Fetch available intervals')).toBe(true)
  })

  it('Multi Quotes counts the symbols, lists the first three and shows Output:', () => {
    const el = face('multiQuotes', {
      symbols: 'SBIN,INFY,TCS,,HDFCBANK',
      outputVariable: 'quotes',
    })
    expect(row(el, 'Symbols: ')).toBe('4')
    expect(has(el, 'SBIN, INFY, TCS...')).toBe(true)
    expect(row(el, 'Output: ')).toBe('quotes')
    const two = face('multiQuotes', { symbols: 'SBIN,INFY' })
    expect(has(two, 'SBIN, INFY')).toBe(true)
    expect(has(two, 'Output: ')).toBe(false)
    expect(row(face('multiQuotes'), 'Symbols: ')).toBe('0')
  })

  it('Option Symbol reads "{optionType} {offset}" with underlying and expiry rows', () => {
    const el = face('optionSymbol', {
      optionType: 'PE',
      offset: 'ITM1',
      underlying: 'BANKNIFTY',
      expiryDate: '28AUG26',
    })
    expect(subtitle(el, 'Option Symbol')).toBe('PE ITM1')
    expect(row(el, 'Underlying')).toBe('BANKNIFTY')
    expect(row(el, 'Expiry')).toBe('28AUG26')
    const blank = face('optionSymbol')
    expect(subtitle(blank, 'Option Symbol')).toBe('CE ATM')
    expect(row(blank, 'Underlying')).toBe('-')
    expect(row(blank, 'Expiry')).toBe('-')
  })

  it.each([
    ['orderBook', 'Order Book', 'All orders'],
    ['tradeBook', 'Trade Book', 'Executed trades'],
    ['positionBook', 'Position Book', 'All positions'],
    ['holdings', 'Holdings', 'Portfolio'],
    ['funds', 'Funds', 'Account Balance'],
    ['margin', 'Margin Calc', 'Risk Check'],
    ['holidays', 'Holidays', 'Market holidays'],
    ['timings', 'Timings', 'Market hours'],
    ['calendar', 'Calendar', 'new day / week / month'],
    ['strategyPnl', 'Strategy P&L', 'realized + unrealized'],
  ])('%s reads "%s / %s"', (type, title, shown) => {
    expect(subtitle(face(type), title)).toBe(shown)
  })

  it.each([
    ['syntheticFuture', 'Synthetic Future'],
    ['optionChain', 'Option Chain'],
  ])('%s shows its exchange, NSE_INDEX by default, with underlying and expiry', (type, title) => {
    const el = face(type, { exchange: 'BSE_INDEX', underlying: 'SENSEX', expiryDate: '28AUG26' })
    expect(subtitle(el, title)).toBe('BSE_INDEX')
    expect(row(el, 'Underlying')).toBe('SENSEX')
    expect(row(el, 'Expiry')).toBe('28AUG26')
    const blank = face(type)
    expect(subtitle(blank, title)).toBe('NSE_INDEX')
    expect(row(blank, 'Underlying')).toBe('-')
    expect(row(blank, 'Expiry')).toBe('-')
  })

  it('Option Chain shows the strike count only when set', () => {
    expect(row(face('optionChain', { strikeCount: 10 }), 'Strikes')).toBe('10')
    expect(has(face('optionChain'), 'Strikes')).toBe(false)
  })

  it.each(['holdings', 'funds'])('%s shows Output: as a {{variable}} only when set', (type) => {
    expect(row(face(type, { outputVariable: 'acct' }), 'Output:')).toBe('{{acct}}')
    expect(has(face(type), 'Output:')).toBe(false)
  })

  it('Margin Calc counts the positions in the basket and shows Output:', () => {
    const basket = JSON.stringify([{ symbol: 'SBIN' }, { symbol: 'INFY' }])
    const el = face('margin', { positionsJson: basket, outputVariable: 'margin' })
    expect(row(el, 'Positions:')).toBe('2')
    expect(row(el, 'Output:')).toBe('{{margin}}')
    const blank = face('margin')
    expect(row(blank, 'Positions:')).toBe('0')
    expect(has(blank, 'Output:')).toBe(false)
  })

  it('Holidays shows the year, this year by default', () => {
    expect(row(face('holidays', { year: 2025 }), 'Year')).toBe('2025')
    expect(row(face('holidays'), 'Year')).toBe(String(new Date().getFullYear()))
  })

  it('Timings shows the date, Today by default; Calendar shows it, today by default', () => {
    expect(row(face('timings', { date: '2026-08-28' }), 'Date')).toBe('2026-08-28')
    expect(row(face('timings'), 'Date')).toBe('Today')
    expect(row(face('calendar', { date: '2026-08-28' }), 'Date')).toBe('2026-08-28')
    expect(row(face('calendar'), 'Date')).toBe('today')
  })
})

describe('IND-085 the Indicator node', () => {
  it('titles itself with the indicator name in capitals, SMA when unset', () => {
    expect(has(face('indicator', { indicatorName: 'rsi' }), 'RSI')).toBe(true)
    expect(has(face('indicator', { indicatorName: 'supertrend' }), 'SUPERTREND')).toBe(true)
    expect(has(face('indicator'), 'SMA')).toBe(true)
  })

  it('reads the interval under the title, D when unset, and the symbol row, - when unset', () => {
    const el = face('indicator', { indicatorName: 'ema', interval: '5m', symbol: 'SBIN' })
    expect(subtitle(el, 'EMA')).toBe('5m')
    expect(row(el, 'Symbol')).toBe('SBIN')
    const blank = face('indicator', { indicatorName: 'ema' })
    expect(subtitle(blank, 'EMA')).toBe('D')
    expect(row(blank, 'Symbol')).toBe('-')
    expect(has(blank, 'Source')).toBe(false)
  })

  it('reads Nested and a Source row when it runs on another series', () => {
    const el = face('indicator', {
      indicatorName: 'sma',
      interval: '5m',
      symbol: 'SBIN',
      sourceSeries: '{{rsi_14}}',
    })
    expect(subtitle(el, 'SMA')).toBe('Nested')
    expect(row(el, 'Source')).toBe('{{rsi_14}}')
    expect(has(el, 'Symbol')).toBe(false)
  })

  it('truncates the source or symbol at 70px and shows the output variable when set', () => {
    const el = face('indicator', { symbol: 'NIFTY25AUG26FUT', outputVariable: 'sma_20' })
    const value = leaf(el, 'NIFTY25AUG26FUT', 'span')
    expect(value.classList.contains('truncate')).toBe(true)
    expect(value.style.maxWidth).toBe('70px')
    expect(has(el, 'sma_20')).toBe(true)
    expect(face('indicator').querySelector('.mt-1')).toBeNull()
  })
})

describe('F-682 the streaming nodes', () => {
  it.each([
    ['subscribeLtp', 'Subscribe LTP', 'Real-time'],
    ['subscribeQuote', 'Subscribe Quote', 'Real-time OHLC'],
    ['subscribeDepth', 'Subscribe Depth', 'Order Book'],
  ])('%s reads "%s / %s" with Symbol:, Exchange: and Output: rows', (type, title, shown) => {
    const el = face(type, { symbol: 'SBIN', exchange: 'BSE', outputVariable: 'tick' })
    expect(subtitle(el, title)).toBe(shown)
    expect(row(el, 'Symbol:')).toBe('SBIN')
    expect(row(el, 'Exchange:')).toBe('BSE')
    expect(row(el, 'Output:')).toBe('{{tick}}')

    const noOutput = face(type, { symbol: 'SBIN' })
    expect(row(noOutput, 'Exchange:')).toBe('NSE')
    expect(has(noOutput, 'Output:')).toBe(false)

    const blank = face(type, { exchange: 'BSE', outputVariable: 'tick' })
    expect(has(blank, 'Symbol:')).toBe(false)
    expect(has(blank, 'Exchange:')).toBe(false)
    expect(has(blank, 'Output:')).toBe(false)
  })

  it.each([
    ['ltp', 'LTP'],
    ['quote', 'Quote'],
    ['depth', 'Depth'],
    ['all', 'All Streams'],
    [undefined, 'All Streams'],
  ])('Unsubscribe reads a %s stream as Type: %s', (streamType, shown) => {
    const el = face('unsubscribe', streamType === undefined ? {} : { streamType })
    expect(subtitle(el, 'Unsubscribe')).toBe('Stop Stream')
    expect(row(el, 'Type:')).toBe(shown)
  })

  it('Unsubscribe shows Symbol: only when set', () => {
    expect(row(face('unsubscribe', { symbol: 'SBIN' }), 'Symbol:')).toBe('SBIN')
    expect(has(face('unsubscribe'), 'Symbol:')).toBe(false)
  })
})
