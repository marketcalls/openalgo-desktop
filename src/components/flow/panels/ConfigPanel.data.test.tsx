/**
 * The data nodes' config forms, as the web draws them: labels, placeholders,
 * hints, the exchange each form falls back to, Select choices, and what an
 * edit stores on the node. Pins parity F-560 to F-563, F-569 to F-585.
 *
 * Every hint below is a reference a trader copies into a later node, so a
 * changed word is a changed workflow: the strings are the source's own.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { DEFAULT_NODE_DATA } from '@/lib/flow/constants'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
  getSymbolLotSizes: vi.fn(async () => ({})),
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
function under<K extends 'input' | 'textarea' | 'button'>(
  label: string,
  tag: K
): HTMLElementTagNameMap[K] {
  const el = screen.getByText(label, { selector: 'label' }).parentElement?.querySelector(tag)
  if (!el) throw new Error(`no ${tag} under ${label}`)
  return el
}

const inputUnder = (label: string) => under(label, 'input')
const shownUnder = (label: string) => under(label, 'button').textContent

/** Opens the Select under `label` and reads its choices as the trader sees them. */
async function choicesUnder(label: string): Promise<string[]> {
  await userEvent.click(under(label, 'button'))
  return (await screen.findAllByRole('option')).map((o) => o.textContent ?? '')
}

async function choose(label: string, option: string) {
  await userEvent.click(under(label, 'button'))
  await userEvent.click(await screen.findByRole('option', { name: option }))
}

/** A text box under `label` showing `placeholder`, whose edit lands in `key`. */
function expectTextField(label: string, placeholder: string, key: string) {
  const input = inputUnder(label)
  expect(input).toHaveValue('')
  expect(input).toHaveAttribute('placeholder', placeholder)
  fireEvent.change(input, { target: { value: 'TYPED1' } })
  expect(stored()[key]).toBe('TYPED1')
}

/** An empty Output Variable shows its name as a hint and stores what is typed. */
function expectOutput(name: string) {
  const output = inputUnder('Output Variable')
  expect(output).toHaveValue('')
  expect(output).toHaveAttribute('placeholder', name)
  fireEvent.change(output, { target: { value: `${name}2` } })
  expect(stored().outputVariable).toBe(`${name}2`)
}

/** Switching a {{reference}} exchange back to the picker lands on `fallback`. */
function expectExchangeFallback(type: string, fallback: string) {
  openPanel(type, { exchange: '{{webhook.exchange}}' })
  expect(inputUnder('Exchange')).toHaveValue('{{webhook.exchange}}')
  fireEvent.click(screen.getByRole('button', { name: 'Use a fixed Exchange' }))
  expect(stored().exchange).toBe(fallback)
}

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('getQuote (F-560)', () => {
  it('asks for a symbol on NSE and says how to read the quote', () => {
    openPanel('getQuote')
    expectTextField('Symbol', 'RELIANCE', 'symbol')
    expect(shownUnder('Exchange')).toBe('NSE')
    expectOutput('quote')
    expect(screen.getByText('Use {{quote.data.ltp}}')).toBeInTheDocument()
  })
})

describe('getDepth (F-561)', () => {
  it('asks for a symbol on NSE and says how to read the best bid', () => {
    openPanel('getDepth')
    expectTextField('Symbol', 'SBIN', 'symbol')
    expect(shownUnder('Exchange')).toBe('NSE')
    expectOutput('depth')
    expect(screen.getByText('Use {{depth.data.bids[0].price}}')).toBeInTheDocument()
  })

  it('stores the exchange the trader picks', async () => {
    openPanel('getDepth', { symbol: 'SBIN' })
    await choose('Exchange', 'BSE')
    expect(stored().exchange).toBe('BSE')
  })
})

describe('getOrderStatus (F-562)', () => {
  it('takes the order id from a Place Order node and says how to read the status', () => {
    openPanel('getOrderStatus')
    expectTextField('Order ID', '{{orderResult.orderid}}', 'orderId')
    expect(screen.getByText('Use variable from Place Order node')).toBeInTheDocument()
    expectOutput('orderStatus')
    expect(screen.getByText('Use {{orderStatus.data.order_status}}')).toBeInTheDocument()
  })
})

describe('openPosition (F-563)', () => {
  it('asks for symbol, exchange and product and says how to read the quantity', () => {
    openPanel('openPosition')
    expectTextField('Symbol', 'RELIANCE', 'symbol')
    expect(shownUnder('Exchange')).toBe('NSE')
    expectOutput('position')
    expect(screen.getByText('Use {{position.quantity}}')).toBeInTheDocument()
  })

  it.each([
    ['NSE', 'MIS'],
    ['NFO', 'NRML'],
  ])('shows the product an %s position defaults to without storing it', (exchange, product) => {
    openPanel('openPosition', { symbol: 'X', exchange })
    expect(shownUnder('Product')).toBe(product)
    expect(stored().product).toBeUndefined()
  })

  it('stores the product the trader picks', async () => {
    openPanel('openPosition', { symbol: 'X', exchange: 'NSE' })
    await choose('Product', 'CNC')
    expect(stored().product).toBe('CNC')
  })
})

describe('strategyPnl (F-569)', () => {
  it("reports this workflow's own P&L when the strategy is blank", () => {
    openPanel('strategyPnl')
    expectTextField('Strategy', "blank = this workflow's name", 'strategy')
    expect(
      screen.getByText(
        "Order nodes tag their orders with the workflow name, so leaving this blank reports this workflow's own P&L."
      )
    ).toBeInTheDocument()
    expectOutput('spnl')
    expect(
      screen.getByText(
        'Exposes {{spnl.realized}}, {{spnl.unrealized}}, {{spnl.total}}, {{spnl.today_realized}}, {{spnl.open_quantity}}.'
      )
    ).toBeInTheDocument()
  })
})

describe('priorPeriodOhlc (F-570)', () => {
  it('asks for a symbol and says which levels it exposes', () => {
    openPanel('priorPeriodOhlc')
    expectTextField('Symbol', 'NIFTY', 'symbol')
    expect(shownUnder('Exchange')).toBe('NSE')
    expectOutput('pdhpdl')
    expect(
      screen.getByText(
        'Exposes {{name.pdh}}, {{name.pdl}}, {{name.pdc}} (also {{name.high}}/low/close/open).'
      )
    ).toBeInTheDocument()
  })

  it('offers four periods, shows the previous day by default and stores the pick', async () => {
    openPanel('priorPeriodOhlc', { symbol: 'NIFTY' })
    expect(shownUnder('Period')).toBe('Previous Day')
    expect(await choicesUnder('Period')).toEqual([
      'Previous Hour',
      'Previous Day',
      'Previous Week',
      'Previous Month',
    ])
    await userEvent.click(screen.getByRole('option', { name: 'Previous Week' }))
    expect(stored().period).toBe('previous_week')
  })

  it('reads from the broker by default and can read Historify instead', async () => {
    openPanel('priorPeriodOhlc', { symbol: 'NIFTY' })
    expect(shownUnder('Source')).toBe('Broker API')
    expect(await choicesUnder('Source')).toEqual(['Broker API', 'Historify DB'])
    await userEvent.click(screen.getByRole('option', { name: 'Historify DB' }))
    expect(stored().source).toBe('db')
  })
})

describe('barOffset (F-571)', () => {
  it('shows a daily interval and the last closed bar when nothing is stored', () => {
    openPanel('barOffset')
    expectTextField('Symbol', 'RELIANCE', 'symbol')
    expect(shownUnder('Exchange')).toBe('NSE')
    const interval = inputUnder('Interval')
    expect(interval).toHaveValue('D')
    expect(interval).toHaveAttribute('placeholder', 'D, 5m, 1h, or a custom Historify interval')
    expect(inputUnder('Bars Back')).toHaveValue(0)
    expect(inputUnder('Bars Back')).toHaveAttribute('min', '0')
    expect(
      screen.getByText('0 = last CLOSED bar, 1 = one bar before that, ...')
    ).toBeInTheDocument()
    expectOutput('bar1')
  })

  it('stores any interval typed, and bars back as a whole number with 0 for blank', () => {
    openPanel('barOffset', { symbol: 'SBIN' })
    fireEvent.change(inputUnder('Interval'), { target: { value: '15m' } })
    expect(stored().interval).toBe('15m')
    fireEvent.change(inputUnder('Bars Back'), { target: { value: '3' } })
    expect(stored().offsetBars).toBe(3)
    fireEvent.change(inputUnder('Bars Back'), { target: { value: '' } })
    expect(stored().offsetBars).toBe(0)
  })

  it('reads from the broker by default and can read Historify instead', async () => {
    openPanel('barOffset', { symbol: 'SBIN' })
    expect(shownUnder('Source')).toBe('Broker API')
    expect(await choicesUnder('Source')).toEqual(['Broker API', 'Historify DB'])
    await userEvent.click(screen.getByRole('option', { name: 'Historify DB' }))
    expect(stored().source).toBe('db')
  })
})

describe('expiry (F-572)', () => {
  it('shows NFO when no exchange is stored and says how to read the first expiry', () => {
    openPanel('expiry')
    expectTextField('Symbol', 'NIFTY', 'symbol')
    expect(shownUnder('Exchange')).toBe('NFO')
    expectOutput('expiries')
    expect(screen.getByText('Use {{expiries.data[0]}}')).toBeInTheDocument()
  })

  it('returns to NFO when a {{reference}} exchange is switched off', () => {
    expectExchangeFallback('expiry', 'NFO')
  })

  it('offers options or futures, options by default, and says why it matters', async () => {
    openPanel('expiry', { symbol: 'NIFTY' })
    expect(shownUnder('Instrument Type')).toBe('Options')
    expect(
      screen.getByText('Futures and options have different expiry calendars')
    ).toBeInTheDocument()
    expect(await choicesUnder('Instrument Type')).toEqual(['Options', 'Futures'])
    await userEvent.click(screen.getByRole('option', { name: 'Futures' }))
    expect(stored().instrumenttype).toBe('futures')
  })
})

describe('multiQuotes (F-573)', () => {
  it('takes comma separated symbols on NSE and says how to read the first quote', () => {
    openPanel('multiQuotes')
    expectTextField('Symbols (comma separated)', 'RELIANCE,INFY,TCS', 'symbols')
    expect(shownUnder('Exchange')).toBe('NSE')
    expect(screen.getByText('Use {{quotes.results[0].data.ltp}}')).toBeInTheDocument()
  })
})

describe('symbol (F-574)', () => {
  it('shows NFO when no exchange is stored and says how to read the lot size', () => {
    openPanel('symbol')
    expectTextField('Symbol', 'NIFTY30DEC25FUT', 'symbol')
    expect(shownUnder('Exchange')).toBe('NFO')
    expectOutput('symbolInfo')
    expect(screen.getByText('Use {{symbolInfo.data.lotsize}}')).toBeInTheDocument()
  })

  it('shows the NSE a new node stores', () => {
    openPanel('symbol', { ...DEFAULT_NODE_DATA.symbol })
    expect(shownUnder('Exchange')).toBe('NSE')
  })

  it('returns to NFO when a {{reference}} exchange is switched off', () => {
    expectExchangeFallback('symbol', 'NFO')
  })
})

describe('optionSymbol (F-575)', () => {
  it('asks for underlying, NSE_INDEX and an expiry date', () => {
    openPanel('optionSymbol')
    expectTextField('Underlying', 'NIFTY', 'underlying')
    expect(shownUnder('Exchange')).toBe('NSE_INDEX')
    expectTextField('Expiry Date', '30DEC25', 'expiryDate')
    expectOutput('optionSym')
  })

  it('returns to NSE_INDEX when a {{reference}} exchange is switched off', () => {
    expectExchangeFallback('optionSymbol', 'NSE_INDEX')
  })

  it('shows ATM when no strike offset is stored, and takes a reference with its rule', () => {
    openPanel('optionSymbol', { underlying: 'NIFTY' })
    expect(shownUnder('Strike Offset')).toBe('ATM')
    fireEvent.click(screen.getByRole('button', { name: 'Take Strike Offset from the payload' }))
    expect(stored().offset).toBe('{{webhook.offset}}')
    expect(inputUnder('Strike Offset')).toHaveValue('{{webhook.offset}}')
    expect(screen.getByText('Must resolve to ATM, ITM1-ITM50 or OTM1-OTM50.')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Use a fixed Strike Offset' }))
    expect(stored().offset).toBe('ATM')
  })

  it('picks Call or Put with plain buttons, none lit until one is stored', () => {
    openPanel('optionSymbol', { underlying: 'NIFTY' })
    const call = screen.getByRole('button', { name: 'Call' })
    const put = screen.getByRole('button', { name: 'Put' })
    expect(call).toHaveClass('bg-muted')
    expect(put).toHaveClass('bg-muted')
    expect(
      screen.queryByRole('button', { name: 'Take Option Type from the payload' })
    ).not.toBeInTheDocument()

    fireEvent.click(put)
    expect(stored().optionType).toBe('PE')
    expect(put).toHaveClass('border-red-500')
    fireEvent.click(call)
    expect(stored().optionType).toBe('CE')
    expect(call).toHaveClass('border-green-500')
    expect(put).toHaveClass('bg-muted')
  })
})

describe('calendar (F-576)', () => {
  it('takes a date, blank for the current session, and lists the flags it exposes', () => {
    openPanel('calendar')
    const date = inputUnder('Date')
    expect(date).toHaveAttribute('type', 'date')
    expect(date).toHaveValue('')
    expect(
      screen.getByText('Leave blank for the current trading session date.')
    ).toBeInTheDocument()
    expect(
      screen.getByText(
        '{{cal.is_new_week}}, {{cal.is_new_month}}, {{cal.is_new_quarter}}, {{cal.is_trading_day}}'
      )
    ).toBeInTheDocument()
    fireEvent.change(date, { target: { value: '2026-11-02' } })
    expect(stored().date).toBe('2026-11-02')
  })
})

describe('intervals (F-577)', () => {
  it('has only an output, and says what it lists', () => {
    openPanel('intervals')
    expect(screen.getAllByRole('textbox')).toHaveLength(1)
    expect(
      screen.getByText('Timeframes this broker supports. Use {{intervals.data.minutes}}')
    ).toBeInTheDocument()
  })
})

describe('orderBook, tradeBook and positionBook (F-578)', () => {
  it.each([
    ['orderBook', 'Use {{orders.data.orders}}'],
    ['tradeBook', 'Use {{trades.data}}'],
    ['positionBook', 'Use {{positions.data}}'],
  ])('%s has only an output, with the hint %s', (type, hint) => {
    openPanel(type)
    expect(screen.getAllByRole('textbox')).toHaveLength(1)
    expect(screen.getByText(hint)).toBeInTheDocument()
  })
})

describe('syntheticFuture (F-579)', () => {
  it('asks for underlying, NSE_INDEX and an expiry date and says how to read the price', () => {
    openPanel('syntheticFuture')
    expectTextField('Underlying', 'NIFTY', 'underlying')
    expect(shownUnder('Exchange')).toBe('NSE_INDEX')
    expectTextField('Expiry Date', '25NOV25', 'expiryDate')
    expectOutput('synthFuture')
    expect(screen.getByText('Use {{synthFuture.synthetic_future_price}}')).toBeInTheDocument()
  })

  it('returns to NSE_INDEX when a {{reference}} exchange is switched off', () => {
    expectExchangeFallback('syntheticFuture', 'NSE_INDEX')
  })
})

describe('optionChain (F-580)', () => {
  it('asks for underlying, NSE_INDEX and an expiry date and says how to read the ATM strike', () => {
    openPanel('optionChain')
    expectTextField('Underlying', 'NIFTY', 'underlying')
    expect(shownUnder('Exchange')).toBe('NSE_INDEX')
    expectTextField('Expiry Date', '30DEC25', 'expiryDate')
    expectOutput('chain')
    expect(screen.getByText('Use {{chain.atm_strike}}')).toBeInTheDocument()
  })

  it('returns to NSE_INDEX when a {{reference}} exchange is switched off', () => {
    expectExchangeFallback('optionChain', 'NSE_INDEX')
  })

  it('stores a whole strike count, and no count for blank (the full chain)', () => {
    openPanel('optionChain', { underlying: 'NIFTY' })
    const count = inputUnder('Strike Count')
    expect(count).toHaveAttribute('type', 'number')
    expect(count).toHaveAttribute('placeholder', '10 (empty=full)')
    expect(count).toHaveValue(null)
    fireEvent.change(count, { target: { value: '15' } })
    expect(stored().strikeCount).toBe(15)
    expect(count).toHaveValue(15)
    fireEvent.change(count, { target: { value: '' } })
    expect(stored().strikeCount).toBeUndefined()
  })
})

describe('holidays (F-581)', () => {
  afterEach(() => vi.useRealTimers())

  it('shows the current year as a hint and stores no year for blank', () => {
    vi.useFakeTimers({ now: new Date('2031-06-15T10:00:00+05:30'), toFake: ['Date'] })
    openPanel('holidays')
    const year = inputUnder('Year')
    expect(year).toHaveAttribute('type', 'number')
    expect(year).toHaveAttribute('placeholder', '2031')
    expect(year).toHaveValue(null)
    expect(screen.getByText('Empty = current year')).toBeInTheDocument()
    fireEvent.change(year, { target: { value: '2027' } })
    expect(stored().year).toBe(2027)
    fireEvent.change(year, { target: { value: '' } })
    expect(stored().year).toBeUndefined()
  })
})

describe('timings (F-582)', () => {
  it('takes a date, blank for today', () => {
    openPanel('timings')
    const date = inputUnder('Date')
    expect(date).toHaveAttribute('type', 'date')
    expect(date).toHaveValue('')
    expect(screen.getByText('Empty = today')).toBeInTheDocument()
    fireEvent.change(date, { target: { value: '2026-11-04' } })
    expect(stored().date).toBe('2026-11-04')
  })
})

describe('holdings and funds (F-583)', () => {
  it.each([
    ['holdings', 'Use {{holdings.data[0].symbol}}'],
    ['funds', 'Use {{funds.data.availablecash}}'],
  ])('%s has only an output, with the hint %s', (type, hint) => {
    openPanel(type)
    expect(screen.getAllByRole('textbox')).toHaveLength(1)
    expect(screen.getByText(hint)).toBeInTheDocument()
  })
})

describe('margin (F-584)', () => {
  it('edits the stored positionsJson text', () => {
    const raw = '[{"symbol":]'
    openPanel('margin', { positionsJson: raw })
    const editor = screen.getByLabelText('Positions JSON')
    expect(editor).toHaveValue(raw)
    fireEvent.change(editor, { target: { value: '[]' } })
    expect(stored().positionsJson).toBe('[]')
  })

  it('stores an added position as JSON text in positionsJson', async () => {
    openPanel('margin')
    expect(
      screen.getByText('No positions yet. Margin is estimated for the basket as a whole.')
    ).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: /add position/i }))
    const written = stored().positionsJson
    expect(typeof written).toBe('string')
    expect(JSON.parse(written as string)).toHaveLength(1)
  })

  it('shows the output name as a hint', () => {
    openPanel('margin')
    expectOutput('marginResult')
  })
})

describe('output names show what is stored, in all fifteen boxes (F-585)', () => {
  const boxes = [
    ['multiQuotes', 'quotes'],
    ['calendar', 'cal'],
    ['intervals', 'intervals'],
    ['orderBook', 'orders'],
    ['tradeBook', 'trades'],
    ['positionBook', 'positions'],
    ['holidays', 'holidays'],
    ['timings', 'timings'],
    ['holdings', 'holdings'],
    ['funds', 'funds'],
    ['margin', 'marginResult'],
    ['mathExpression', 'result'],
    ['subscribeLtp', 'ltp'],
    ['subscribeQuote', 'quote'],
    ['subscribeDepth', 'depth'],
  ]

  it.each(boxes)('a %s with no name stored shows %s as a hint, not a value', (type, name) => {
    openPanel(type)
    expectOutput(name)
  })

  it.each(boxes)('a %s shows the name it stores', (type) => {
    openPanel(type, { outputVariable: 'mine' })
    expect(inputUnder('Output Variable')).toHaveValue('mine')
  })

  it.each(boxes)('clearing a %s name stores the empty name', (type) => {
    openPanel(type, { outputVariable: 'mine' })
    fireEvent.change(inputUnder('Output Variable'), { target: { value: '' } })
    expect(stored().outputVariable).toBe('')
    expect(inputUnder('Output Variable')).toHaveValue('')
  })
})
