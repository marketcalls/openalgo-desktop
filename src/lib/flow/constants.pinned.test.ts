/**
 * The editor's constant lists and node defaults, as the web ships them
 * (parity F-240, F-241, F-243, F-245..F-249, F-252..F-258, C-13, IND-080,
 * IND-081). src/lib/flow/constants.ts is byte-identical to the web's; these
 * pin every value, label and description a trader sees in a Select or a new
 * node, so a change to any of them is a deliberate change to this file too.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import {
  BSE_INDEX_SYMBOLS,
  CONDITION_OPERATORS,
  DAYS_OF_WEEK,
  DEFAULT_NODE_DATA,
  EXCHANGES,
  EXPIRY_TYPES,
  GREEKS,
  INDEX_EXCHANGES,
  INDICATOR_CATALOG,
  INDICATOR_CATEGORIES,
  MAX_STRIKE_OFFSET,
  NODE_CATEGORIES,
  NODE_DEFINITIONS,
  NSE_INDEX_SYMBOLS,
  OPTION_STRATEGIES,
  OPTION_TYPES,
  ORDER_ACTIONS,
  POSITION_CONDITIONS,
  PRICE_ALERT_CONDITIONS,
  PRICE_TYPES,
  PRODUCT_TYPES,
  SCHEDULE_TYPES,
  STRIKE_OFFSETS,
  strikeOffsetOptions,
} from './constants'

describe('choice lists', () => {
  it('EXCHANGES (F-240)', () => {
    expect(EXCHANGES).toEqual([
      { value: 'NSE', label: 'NSE' },
      { value: 'BSE', label: 'BSE' },
      { value: 'NFO', label: 'NFO' },
      { value: 'BFO', label: 'BFO' },
      { value: 'CDS', label: 'CDS' },
      { value: 'BCD', label: 'BCD' },
      { value: 'MCX', label: 'MCX' },
      { value: 'NCDEX', label: 'NCDEX' },
      { value: 'NCO', label: 'NCO' },
      { value: 'NSE_INDEX', label: 'NSE_INDEX' },
      { value: 'BSE_INDEX', label: 'BSE_INDEX' },
      { value: 'MCX_INDEX', label: 'MCX_INDEX' },
      { value: 'GLOBAL_INDEX', label: 'GLOBAL_INDEX' },
      { value: 'CRYPTO', label: 'CRYPTO' },
    ])
  })

  it('INDEX_EXCHANGES (F-240)', () => {
    expect(INDEX_EXCHANGES).toEqual([
      { value: 'NSE_INDEX', label: 'NSE_INDEX' },
      { value: 'BSE_INDEX', label: 'BSE_INDEX' },
      { value: 'MCX_INDEX', label: 'MCX_INDEX' },
      { value: 'GLOBAL_INDEX', label: 'GLOBAL_INDEX' },
    ])
  })

  it('PRODUCT_TYPES (F-241)', () => {
    expect(PRODUCT_TYPES).toEqual([
      { value: 'MIS', label: 'MIS', description: 'Intraday (auto square-off)' },
      { value: 'CNC', label: 'CNC', description: 'Cash & Carry for equity delivery' },
      { value: 'NRML', label: 'NRML', description: 'Normal for futures and options' },
    ])
  })

  it('PRICE_TYPES (F-243)', () => {
    expect(PRICE_TYPES).toEqual([
      { value: 'MARKET', label: 'Market', description: 'Execute at current market price' },
      { value: 'LIMIT', label: 'Limit', description: 'Execute at specified price or better' },
      { value: 'SL', label: 'Stop Loss Limit', description: 'Stop loss with limit price' },
      { value: 'SL-M', label: 'Stop Loss Market', description: 'Stop loss at market price' },
    ])
  })

  it('ORDER_ACTIONS (F-243)', () => {
    expect(ORDER_ACTIONS).toEqual([
      { value: 'BUY', label: 'BUY', color: 'badge-buy' },
      { value: 'SELL', label: 'SELL', color: 'badge-sell' },
    ])
  })

  it('OPTION_TYPES (F-243)', () => {
    expect(OPTION_TYPES).toEqual([
      { value: 'CE', label: 'Call (CE)', description: 'Call Option' },
      { value: 'PE', label: 'Put (PE)', description: 'Put Option' },
    ])
  })

  it('OPTION_STRATEGIES (F-245)', () => {
    expect(OPTION_STRATEGIES).toEqual([
      {
        value: 'iron_condor',
        label: 'Iron Condor',
        description: 'Sell OTM Call & Put, Buy further OTM Call & Put',
      },
      { value: 'straddle', label: 'Straddle', description: 'Buy/Sell ATM Call and Put' },
      { value: 'strangle', label: 'Strangle', description: 'Buy/Sell OTM Call and Put' },
      {
        value: 'bull_call_spread',
        label: 'Bull Call Spread',
        description: 'Buy lower strike Call, Sell higher strike Call',
      },
      {
        value: 'bear_put_spread',
        label: 'Bear Put Spread',
        description: 'Buy higher strike Put, Sell lower strike Put',
      },
      { value: 'custom', label: 'Custom', description: 'Build custom multi-leg strategy' },
    ])
  })

  it('NSE_INDEX_SYMBOLS (F-246)', () => {
    expect(NSE_INDEX_SYMBOLS).toEqual([
      { value: 'NIFTY', label: 'NIFTY 50' },
      { value: 'BANKNIFTY', label: 'Bank NIFTY' },
      { value: 'FINNIFTY', label: 'Fin NIFTY' },
      { value: 'MIDCPNIFTY', label: 'Midcap NIFTY' },
      { value: 'NIFTYNXT50', label: 'NIFTY Next 50' },
    ])
  })

  it('BSE_INDEX_SYMBOLS (F-246)', () => {
    expect(BSE_INDEX_SYMBOLS).toEqual([
      { value: 'SENSEX', label: 'SENSEX' },
      { value: 'BANKEX', label: 'BANKEX' },
      { value: 'SENSEX50', label: 'SENSEX 50' },
    ])
  })

  it('EXPIRY_TYPES (F-247)', () => {
    expect(EXPIRY_TYPES).toEqual([
      { value: 'current_week', label: 'Current Week', description: 'Nearest weekly expiry' },
      { value: 'next_week', label: 'Next Week', description: 'Second weekly expiry' },
      {
        value: 'current_month',
        label: 'Current Month',
        description: 'Last expiry of current month',
      },
      { value: 'next_month', label: 'Next Month', description: 'Last expiry of next month' },
    ])
  })

  it('SCHEDULE_TYPES (F-248)', () => {
    expect(SCHEDULE_TYPES).toEqual([
      { value: 'once', label: 'Once', description: 'Execute one time at specified date/time' },
      { value: 'daily', label: 'Daily', description: 'Execute every day at specified time' },
      { value: 'weekly', label: 'Weekly', description: 'Execute on selected days of the week' },
      { value: 'interval', label: 'Interval', description: 'Execute every X minutes' },
    ])
  })

  it('DAYS_OF_WEEK (F-248)', () => {
    expect(DAYS_OF_WEEK).toEqual([
      { value: 0, label: 'Mon', fullLabel: 'Monday' },
      { value: 1, label: 'Tue', fullLabel: 'Tuesday' },
      { value: 2, label: 'Wed', fullLabel: 'Wednesday' },
      { value: 3, label: 'Thu', fullLabel: 'Thursday' },
      { value: 4, label: 'Fri', fullLabel: 'Friday' },
      { value: 5, label: 'Sat', fullLabel: 'Saturday' },
      { value: 6, label: 'Sun', fullLabel: 'Sunday' },
    ])
  })

  it('CONDITION_OPERATORS (F-249)', () => {
    expect(CONDITION_OPERATORS).toEqual([
      { value: '>', label: '>', description: 'Greater than' },
      { value: '<', label: '<', description: 'Less than' },
      { value: '==', label: '=', description: 'Equal to' },
      { value: '>=', label: '>=', description: 'Greater than or equal' },
      { value: '<=', label: '<=', description: 'Less than or equal' },
      { value: '!=', label: '!=', description: 'Not equal to' },
    ])
  })

  it('PRICE_ALERT_CONDITIONS (F-249)', () => {
    expect(PRICE_ALERT_CONDITIONS).toEqual([
      { value: 'above', label: 'Price Above', description: 'Trigger when price goes above' },
      { value: 'below', label: 'Price Below', description: 'Trigger when price goes below' },
      {
        value: 'crosses_above',
        label: 'Crosses Above',
        description: 'Trigger when price crosses above',
      },
      {
        value: 'crosses_below',
        label: 'Crosses Below',
        description: 'Trigger when price crosses below',
      },
    ])
  })

  it('POSITION_CONDITIONS (F-249)', () => {
    expect(POSITION_CONDITIONS).toEqual([
      { value: 'exists', label: 'Position Exists', description: 'Has an open position' },
      { value: 'not_exists', label: 'No Position', description: 'No open position' },
      {
        value: 'quantity_above',
        label: 'Qty Above',
        description: 'Position quantity above threshold',
      },
      {
        value: 'quantity_below',
        label: 'Qty Below',
        description: 'Position quantity below threshold',
      },
      { value: 'pnl_above', label: 'P&L Above', description: 'Position P&L above threshold' },
      { value: 'pnl_below', label: 'P&L Below', description: 'Position P&L below threshold' },
    ])
  })

  it('GREEKS (F-249)', () => {
    expect(GREEKS).toEqual([
      { value: 'delta', label: 'Delta', description: 'Price sensitivity' },
      { value: 'gamma', label: 'Gamma', description: 'Delta sensitivity' },
      { value: 'theta', label: 'Theta', description: 'Time decay' },
      { value: 'vega', label: 'Vega', description: 'Volatility sensitivity' },
      { value: 'iv', label: 'IV', description: 'Implied Volatility' },
    ])
  })

  it('DAYS_OF_WEEK numbers Monday 0 to Sunday 6, as the scheduler reads them (F-248)', () => {
    expect(DAYS_OF_WEEK.map((day) => [day.value, day.fullLabel])).toEqual([
      [0, 'Monday'],
      [1, 'Tuesday'],
      [2, 'Wednesday'],
      [3, 'Thursday'],
      [4, 'Friday'],
      [5, 'Saturday'],
      [6, 'Sunday'],
    ])
  })
})

describe('strike offsets (C-13)', () => {
  it('stop at 25 strikes, and keep a stored value the list does not carry', () => {
    expect(MAX_STRIKE_OFFSET).toBe(25)
    expect(strikeOffsetOptions('OTM40')[0]).toEqual({
      value: 'OTM40',
      label: 'OTM40',
      description: 'Stored on this node',
    })
  })
})

describe('the node catalogue (F-252)', () => {
  it('files each node under its category with the web label and description', () => {
    const rows = Object.fromEntries(
      Object.entries(NODE_DEFINITIONS).map(([category, defs]) => [
        category,
        defs.map((d) => [d.type, d.label, d.description, d.category]),
      ])
    )
    expect(rows).toEqual({
      TRIGGERS: [
        ['start', 'Schedule', 'Start workflow on schedule', 'trigger'],
        ['priceAlert', 'Price Alert', 'Trigger on price condition', 'trigger'],
        ['webhookTrigger', 'Webhook', 'Trigger from external webhook', 'trigger'],
        ['orderUpdateTrigger', 'Order Update', 'Trigger on order status change', 'trigger'],
      ],
      ACTIONS: [
        ['placeOrder', 'Place Order', 'Place a trading order', 'action'],
        ['smartOrder', 'Smart Order', 'Position-aware order', 'action'],
        ['optionsOrder', 'Options Order', 'Trade ATM/ITM/OTM options', 'action'],
        ['optionsMultiOrder', 'Options Strategy', 'Multi-leg options strategy', 'action'],
        ['basketOrder', 'Basket Order', 'Place multiple orders at once', 'action'],
        ['splitOrder', 'Split Order', 'Split large order into chunks', 'action'],
        ['modifyOrder', 'Modify Order', 'Modify an existing order', 'action'],
        ['cancelOrder', 'Cancel Order', 'Cancel a specific order', 'action'],
        ['cancelAllOrders', 'Cancel All', 'Cancel all open orders', 'action'],
        ['closePositions', 'Close Positions', 'Square off all positions', 'action'],
      ],
      CONDITIONS: [
        ['positionCheck', 'Position Check', 'Check position status', 'condition'],
        ['fundCheck', 'Fund Check', 'Check available funds', 'condition'],
        ['priceCondition', 'Price Check', 'Check price condition', 'condition'],
        ['timeWindow', 'Time Window', 'Check market hours', 'condition'],
        ['timeCondition', 'Time Condition', 'Check time condition', 'condition'],
        ['andGate', 'AND Gate', 'All conditions must be true', 'condition'],
        ['orGate', 'OR Gate', 'Any condition must be true', 'condition'],
        ['notGate', 'NOT Gate', 'Invert condition result', 'condition'],
        ['varCondition', 'Var Condition', 'Compare any two values', 'condition'],
      ],
      DATA: [
        ['getQuote', 'Get Quote', 'Fetch real-time quote', 'data'],
        ['getDepth', 'Market Depth', 'Fetch bid/ask depth', 'data'],
        ['history', 'History', 'Fetch OHLCV data', 'data'],
        ['indicator', 'Indicator', 'Run any technical indicator', 'data'],
        ['strategyPnl', 'Strategy P&L', 'Realized/unrealized P&L for a strategy', 'data'],
        ['priorPeriodOhlc', 'Prior Period OHLC', 'Previous hour/day/week/month candle', 'data'],
        ['barOffset', 'Bar Offset', 'OHLCV N bars back', 'data'],
        ['openPosition', 'Open Position', 'Get position for symbol', 'data'],
        ['getOrderStatus', 'Order Status', 'Check order status', 'data'],
        ['expiry', 'Expiry Dates', 'F&O expiry dates', 'data'],
        ['intervals', 'Intervals', 'Broker-supported timeframes', 'data'],
        ['multiQuotes', 'Multi Quotes', 'Quotes for several symbols', 'data'],
        ['symbol', 'Symbol Info', 'Lot size, tick size, token', 'data'],
        ['optionSymbol', 'Option Symbol', 'Resolve an ATM-relative strike', 'data'],
        ['syntheticFuture', 'Synthetic Future', 'Synthetic future price', 'data'],
        ['optionChain', 'Option Chain', 'Strikes with CE and PE', 'data'],
        ['margin', 'Margin', 'Margin required for a position or basket', 'data'],
        ['holidays', 'Holidays', 'Exchange holiday list for a year', 'data'],
        ['timings', 'Market Timings', 'Market open and close for a date', 'data'],
        ['calendar', 'Calendar', 'New day, week, month or quarter', 'data'],
        ['subscribeLtp', 'Subscribe LTP', 'Stream last traded price', 'streaming'],
        ['subscribeQuote', 'Subscribe Quote', 'Stream full quote', 'streaming'],
        ['subscribeDepth', 'Subscribe Depth', 'Stream market depth', 'streaming'],
        ['unsubscribe', 'Unsubscribe', 'Stop a stream', 'streaming'],
        ['mathExpression', 'Math Expression', 'Arithmetic over variables', 'utility'],
        ['group', 'Group', 'Visual grouping only', 'utility'],
        ['orderBook', 'Order Book', 'Get all orders', 'data'],
        ['tradeBook', 'Trade Book', 'Get all trades', 'data'],
        ['positionBook', 'Position Book', 'Get all positions', 'data'],
        ['holdings', 'Holdings', 'Get portfolio holdings', 'data'],
        ['funds', 'Funds', 'Get available funds', 'data'],
      ],
      UTILITIES: [
        ['telegramAlert', 'Telegram Alert', 'Send Telegram notification', 'utility'],
        ['whatsappAlert', 'WhatsApp Alert', 'Send WhatsApp notification', 'utility'],
        ['delay', 'Delay', 'Wait for duration', 'utility'],
        ['waitUntil', 'Wait Until', 'Wait until specific time', 'utility'],
        ['log', 'Log', 'Log a message', 'utility'],
        ['variable', 'Variable', 'Set/calculate variable', 'utility'],
        ['httpRequest', 'HTTP Request', 'Make HTTP request', 'utility'],
      ],
    })
  })

  it('names the five categories', () => {
    expect(NODE_CATEGORIES).toEqual({
      TRIGGERS: 'triggers',
      ACTIONS: 'actions',
      CONDITIONS: 'conditions',
      DATA: 'data',
      UTILITIES: 'utilities',
    })
  })

  it("lists exactly the backend validator's node types", () => {
    const catalog = readFileSync(
      join(process.cwd(), 'src-tauri/src/flow/validator/catalog.rs'),
      'utf8'
    )
    const start = catalog.indexOf('pub const VALID_NODE_TYPES')
    const backend = [...catalog.slice(start, catalog.indexOf('];', start)).matchAll(/"(\w+)"/g)]
    const editor = Object.values(NODE_DEFINITIONS)
      .flat()
      .map((d) => d.type)
    expect(editor.sort()).toEqual(backend.map((m) => m[1]).sort())
  })
})

describe('what a new node starts with (F-253..F-258, IND-081)', () => {
  it('triggers (F-253)', () => {
    expect(DEFAULT_NODE_DATA.start).toEqual({
      scheduleType: 'daily',
      time: '09:15',
      marketHoursOnly: true,
      marketHoursStart: '09:15',
      marketHoursEnd: '15:15',
      marketHoursExchange: 'NSE',
    })
    expect(DEFAULT_NODE_DATA.priceAlert).toEqual({
      symbol: '',
      exchange: 'NSE',
      condition: 'above',
      price: 0,
      trigger: 'once',
      expiration: 'none',
    })
    expect(DEFAULT_NODE_DATA.orderUpdateTrigger).toEqual({
      orderId: '',
      symbol: '',
      exchange: '',
      status: 'complete',
      trigger: 'once',
    })
    expect(DEFAULT_NODE_DATA.webhookTrigger).toEqual({ label: '' })
  })

  it('orders (F-254)', () => {
    expect(DEFAULT_NODE_DATA.placeOrder).toEqual({
      symbol: '',
      exchange: 'NSE',
      action: 'BUY',
      quantity: 1,
      priceType: 'MARKET',
      price: 0,
      triggerPrice: 0,
    })
    expect(DEFAULT_NODE_DATA.smartOrder).toEqual({
      symbol: '',
      exchange: 'NSE',
      action: 'BUY',
      quantity: 1,
      positionSize: 0,
      priceType: 'MARKET',
      price: 0,
      triggerPrice: 0,
    })
    expect(DEFAULT_NODE_DATA.optionsOrder).toEqual({
      underlying: 'NIFTY',
      exchange: 'NSE_INDEX',
      expiryType: 'current_week',
      offset: 'ATM',
      optionType: 'CE',
      action: 'BUY',
      quantity: 1,
      priceType: 'MARKET',
      product: 'NRML',
      price: 0,
      triggerPrice: 0,
    })
    expect(DEFAULT_NODE_DATA.optionsMultiOrder).toEqual({
      strategy: 'straddle',
      underlying: 'NIFTY',
      exchange: 'NSE_INDEX',
      expiryType: 'current_week',
      legs: [],
      action: 'SELL',
      quantity: 1,
      strangleWidth: 'OTM2',
      priceType: 'MARKET',
      price: 0,
      product: 'NRML',
    })
    expect(DEFAULT_NODE_DATA.basketOrder).toEqual({
      orders: '',
      priceType: 'MARKET',
      price: 0,
      triggerPrice: 0,
    })
    expect(DEFAULT_NODE_DATA.splitOrder).toEqual({
      symbol: '',
      exchange: 'NSE',
      action: 'BUY',
      quantity: 100,
      splitSize: 50,
      priceType: 'MARKET',
      price: 0,
      triggerPrice: 0,
    })
    expect(DEFAULT_NODE_DATA.modifyOrder).toEqual({ orderId: '' })
    expect(DEFAULT_NODE_DATA.cancelOrder).toEqual({ orderId: '' })
    expect(DEFAULT_NODE_DATA.cancelAllOrders).toEqual({})
    expect(DEFAULT_NODE_DATA.closePositions).toEqual({ symbol: '', exchange: 'NSE' })
  })

  it('conditions (F-255)', () => {
    expect(DEFAULT_NODE_DATA.positionCheck).toEqual({
      symbol: '',
      exchange: 'NSE',
      condition: 'exists',
      threshold: 0,
    })
    expect(DEFAULT_NODE_DATA.fundCheck).toEqual({ minAvailable: 10000 })
    expect(DEFAULT_NODE_DATA.priceCondition).toEqual({
      symbol: '',
      exchange: 'NSE',
      field: 'ltp',
      operator: '>',
      value: 0,
    })
    expect(DEFAULT_NODE_DATA.varCondition).toEqual({
      leftValue: '',
      operator: '>',
      rightValue: '0',
    })
    expect(DEFAULT_NODE_DATA.timeWindow).toEqual({ startTime: '09:15', endTime: '15:30' })
    expect(DEFAULT_NODE_DATA.timeCondition).toEqual({ targetTime: '09:30', operator: '>=' })
    expect(DEFAULT_NODE_DATA.andGate).toEqual({})
    expect(DEFAULT_NODE_DATA.orGate).toEqual({})
    expect(DEFAULT_NODE_DATA.notGate).toEqual({})
  })

  it('streaming (F-257)', () => {
    expect(DEFAULT_NODE_DATA.subscribeLtp).toEqual({
      symbol: '',
      exchange: 'NSE',
      outputVariable: 'ltp',
    })
    expect(DEFAULT_NODE_DATA.subscribeQuote).toEqual({
      symbol: '',
      exchange: 'NSE',
      outputVariable: 'quote',
    })
    expect(DEFAULT_NODE_DATA.subscribeDepth).toEqual({
      symbol: '',
      exchange: 'NSE',
      outputVariable: 'depth',
    })
    expect(DEFAULT_NODE_DATA.unsubscribe).toEqual({
      symbol: '',
      exchange: 'NSE',
      streamType: 'all',
    })
  })

  it('utilities (F-258)', () => {
    expect(DEFAULT_NODE_DATA.telegramAlert).toEqual({ message: 'Workflow executed successfully' })
    expect(DEFAULT_NODE_DATA.whatsappAlert).toEqual({
      to: '',
      message: 'Workflow executed successfully',
    })
    expect(DEFAULT_NODE_DATA.delay).toEqual({ delayValue: 1, delayUnit: 'seconds' })
    expect(DEFAULT_NODE_DATA.waitUntil).toEqual({ targetTime: '09:30' })
    expect(DEFAULT_NODE_DATA.log).toEqual({ message: 'Log message here', level: 'info' })
    expect(DEFAULT_NODE_DATA.variable).toEqual({
      variableName: 'myVar',
      operation: 'set',
      value: '',
    })
    expect(DEFAULT_NODE_DATA.httpRequest).toEqual({
      method: 'GET',
      url: '',
      headers: '',
      body: '',
      timeout: 30000,
      outputVariable: 'response',
    })
    expect(DEFAULT_NODE_DATA.mathExpression).toEqual({ expression: '', outputVariable: 'result' })
    expect(DEFAULT_NODE_DATA.group).toEqual({})
  })

  it('data nodes (F-256), the indicator included (IND-081)', () => {
    expect(DEFAULT_NODE_DATA.getOrderStatus).toEqual({
      orderId: '',
      waitForCompletion: false,
      outputVariable: 'orderStatus',
    })
    expect(DEFAULT_NODE_DATA.getQuote).toEqual({ symbol: '', exchange: 'NSE', outputVariable: '' })
    expect(DEFAULT_NODE_DATA.getDepth).toEqual({ symbol: '', exchange: 'NSE', outputVariable: '' })
    expect(DEFAULT_NODE_DATA.history).toEqual({
      symbol: '',
      exchange: 'NSE',
      interval: '5m',
      days: 30,
      startDate: '',
      endDate: '',
      outputVariable: '',
    })
    expect(DEFAULT_NODE_DATA.indicator).toEqual({
      symbol: '',
      exchange: 'NSE',
      interval: 'D',
      source: 'api',
      indicatorName: 'rsi',
      params: '{"period": 14}',
      lookbackBars: 100,
      tailBars: 5,
      offsetBars: 0,
      sourceSeries: '',
      sourceField: '',
      outputVariable: 'ind',
    })
    expect(DEFAULT_NODE_DATA.strategyPnl).toEqual({ strategy: '', outputVariable: '' })
    expect(DEFAULT_NODE_DATA.priorPeriodOhlc).toEqual({
      symbol: '',
      exchange: 'NSE',
      period: 'previous_day',
      source: 'api',
      outputVariable: '',
    })
    expect(DEFAULT_NODE_DATA.barOffset).toEqual({
      symbol: '',
      exchange: 'NSE',
      interval: 'D',
      source: 'api',
      offsetBars: 0,
      outputVariable: '',
    })
    expect(DEFAULT_NODE_DATA.openPosition).toEqual({
      symbol: '',
      exchange: 'NSE',
      outputVariable: '',
    })
    expect(DEFAULT_NODE_DATA.orderBook).toEqual({ outputVariable: 'orders' })
    expect(DEFAULT_NODE_DATA.tradeBook).toEqual({ outputVariable: 'trades' })
    expect(DEFAULT_NODE_DATA.positionBook).toEqual({ outputVariable: 'positions' })
    expect(DEFAULT_NODE_DATA.holdings).toEqual({ outputVariable: 'holdings' })
    expect(DEFAULT_NODE_DATA.funds).toEqual({ outputVariable: 'funds' })
    expect(DEFAULT_NODE_DATA.symbol).toEqual({ symbol: '', exchange: 'NSE', outputVariable: '' })
    expect(DEFAULT_NODE_DATA.optionSymbol).toEqual({
      underlying: 'NIFTY',
      exchange: 'NSE_INDEX',
      expiryDate: '',
      offset: 'ATM',
      optionType: 'CE',
      outputVariable: '',
    })
    expect(DEFAULT_NODE_DATA.expiry).toEqual({
      symbol: 'NIFTY',
      exchange: 'NFO',
      outputVariable: '',
    })
    expect(DEFAULT_NODE_DATA.intervals).toEqual({ outputVariable: 'intervals' })
    expect(DEFAULT_NODE_DATA.multiQuotes).toEqual({
      symbols: '',
      exchange: 'NSE',
      outputVariable: 'quotes',
    })
    expect(DEFAULT_NODE_DATA.optionChain).toEqual({
      underlying: 'NIFTY',
      exchange: 'NSE_INDEX',
      expiryDate: '',
      strikeCount: 10,
      outputVariable: '',
    })
    expect(DEFAULT_NODE_DATA.syntheticFuture).toEqual({
      underlying: 'NIFTY',
      exchange: 'NSE_INDEX',
      expiryDate: '',
      outputVariable: '',
    })
    expect(DEFAULT_NODE_DATA.holidays).toStrictEqual({
      year: undefined,
      outputVariable: 'holidays',
    })
    expect(DEFAULT_NODE_DATA.timings).toEqual({ date: '', outputVariable: 'timings' })
    expect(DEFAULT_NODE_DATA.calendar).toEqual({ date: '', outputVariable: 'cal' })
    expect(DEFAULT_NODE_DATA.margin).toEqual({
      symbol: '',
      exchange: 'NSE',
      quantity: 1,
      price: 0,
      action: 'BUY',
      priceType: 'MARKET',
      outputVariable: 'marginResult',
    })
  })
})

describe('the indicator catalogue (IND-080)', () => {
  it('has the nine categories in order and 116 entries, each in one of them', () => {
    expect(INDICATOR_CATEGORIES).toEqual([
      'Trend',
      'Momentum',
      'Volatility',
      'Volume',
      'Oscillators',
      'Statistical',
      'Hybrid',
      'Price Transform',
      'Utility',
    ])
    expect(INDICATOR_CATALOG).toHaveLength(116)
    for (const entry of INDICATOR_CATALOG) {
      expect(INDICATOR_CATEGORIES, entry.value).toContain(entry.category)
      expect(entry.label.trim(), entry.value).not.toBe('')
    }
  })
})

describe('the strike offsets (F-244)', () => {
  it('describes ATM as At The Money', () => {
    expect(STRIKE_OFFSETS[0]).toEqual({ value: 'ATM', label: 'ATM', description: 'At The Money' })
  })

  it('reads a stored offset trimmed: a listed one adds nothing, an unlisted one is offered as stored', () => {
    expect(strikeOffsetOptions('  OTM2 ')).toBe(STRIKE_OFFSETS)
    const options = strikeOffsetOptions(' OTM60 ')
    expect(options[0]).toEqual({
      value: 'OTM60',
      label: 'OTM60',
      description: 'Stored on this node',
    })
    expect(options).toHaveLength(STRIKE_OFFSETS.length + 1)
  })
})
