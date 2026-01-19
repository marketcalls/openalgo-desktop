// Playground API endpoints configuration
// Based on Bruno collections from openalgo Flask app

export interface PlaygroundEndpoint {
  name: string
  path: string
  method: 'GET' | 'POST'
  body?: Record<string, unknown>
  params?: Record<string, unknown>
}

export interface PlaygroundEndpointsByCategory {
  [category: string]: PlaygroundEndpoint[]
}

// Default request body with apikey placeholder
const defaultBody = (extra: Record<string, unknown> = {}) => ({
  apikey: '',
  ...extra,
})

// Order endpoints
const orderEndpoints: PlaygroundEndpoint[] = [
  {
    name: 'Place Order',
    path: '/api/v1/placeorder',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      exchange: 'NSE',
      symbol: 'RELIANCE',
      action: 'BUY',
      product: 'MIS',
      pricetype: 'MARKET',
      quantity: '1',
      price: '0',
      trigger_price: '0',
      disclosed_quantity: '0',
    }),
  },
  {
    name: 'Place Smart Order',
    path: '/api/v1/placesmartorder',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      exchange: 'NSE',
      symbol: 'RELIANCE',
      action: 'BUY',
      product: 'MIS',
      pricetype: 'MARKET',
      quantity: '1',
      position_size: '0',
      price: '0',
      trigger_price: '0',
      disclosed_quantity: '0',
    }),
  },
  {
    name: 'Basket Order',
    path: '/api/v1/basketorder',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      orders: [
        {
          exchange: 'NSE',
          symbol: 'RELIANCE',
          action: 'BUY',
          product: 'MIS',
          pricetype: 'MARKET',
          quantity: '1',
        },
        {
          exchange: 'NSE',
          symbol: 'TCS',
          action: 'SELL',
          product: 'MIS',
          pricetype: 'MARKET',
          quantity: '1',
        },
      ],
    }),
  },
  {
    name: 'Split Order',
    path: '/api/v1/splitorder',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      exchange: 'NFO',
      symbol: 'NIFTY24JAN22000CE',
      action: 'BUY',
      product: 'MIS',
      pricetype: 'MARKET',
      quantity: '2500',
      splitsize: '900',
      price: '0',
      trigger_price: '0',
    }),
  },
  {
    name: 'Modify Order',
    path: '/api/v1/modifyorder',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      orderid: '',
      exchange: 'NSE',
      symbol: 'RELIANCE',
      action: 'BUY',
      product: 'MIS',
      pricetype: 'LIMIT',
      quantity: '1',
      price: '100',
    }),
  },
  {
    name: 'Cancel Order',
    path: '/api/v1/cancelorder',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      orderid: '',
    }),
  },
  {
    name: 'Cancel All Orders',
    path: '/api/v1/cancelallorder',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
    }),
  },
  {
    name: 'Close All Positions',
    path: '/api/v1/closeposition',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
    }),
  },
  {
    name: 'Order Status',
    path: '/api/v1/orderstatus',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      orderid: '',
    }),
  },
  {
    name: 'Open Position',
    path: '/api/v1/openposition',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      exchange: 'NSE',
      symbol: 'RELIANCE',
      product: 'MIS',
    }),
  },
]

// Account endpoints
const accountEndpoints: PlaygroundEndpoint[] = [
  {
    name: 'Funds',
    path: '/api/v1/funds',
    method: 'POST',
    body: defaultBody(),
  },
  {
    name: 'Order Book',
    path: '/api/v1/orderbook',
    method: 'POST',
    body: defaultBody(),
  },
  {
    name: 'Trade Book',
    path: '/api/v1/tradebook',
    method: 'POST',
    body: defaultBody(),
  },
  {
    name: 'Position Book',
    path: '/api/v1/positionbook',
    method: 'POST',
    body: defaultBody(),
  },
  {
    name: 'Holdings',
    path: '/api/v1/holdings',
    method: 'POST',
    body: defaultBody(),
  },
  {
    name: 'Margin Calculator',
    path: '/api/v1/margin',
    method: 'POST',
    body: defaultBody({
      exchange: 'NSE',
      symbol: 'RELIANCE',
      quantity: '10',
      pricetype: 'MARKET',
      side: 'BUY',
      product: 'MIS',
    }),
  },
]

// Data endpoints
const dataEndpoints: PlaygroundEndpoint[] = [
  {
    name: 'Quotes',
    path: '/api/v1/quotes',
    method: 'POST',
    body: defaultBody({
      exchange: 'NSE',
      symbol: 'RELIANCE',
    }),
  },
  {
    name: 'Multi Quotes',
    path: '/api/v1/multiquotes',
    method: 'POST',
    body: defaultBody({
      symbols: [
        { exchange: 'NSE', symbol: 'RELIANCE' },
        { exchange: 'NSE', symbol: 'TCS' },
        { exchange: 'NSE', symbol: 'INFY' },
      ],
    }),
  },
  {
    name: 'Market Depth',
    path: '/api/v1/depth',
    method: 'POST',
    body: defaultBody({
      exchange: 'NSE',
      symbol: 'RELIANCE',
    }),
  },
  {
    name: 'History (EOD)',
    path: '/api/v1/history',
    method: 'POST',
    body: defaultBody({
      exchange: 'NSE',
      symbol: 'RELIANCE',
      interval: 'D',
      start_date: '2024-01-01',
      end_date: '2024-01-31',
    }),
  },
  {
    name: 'History (Intraday)',
    path: '/api/v1/history',
    method: 'POST',
    body: defaultBody({
      exchange: 'NSE',
      symbol: 'RELIANCE',
      interval: '5m',
      start_date: '2024-01-15',
      end_date: '2024-01-15',
    }),
  },
  {
    name: 'Search Symbols',
    path: '/api/v1/search',
    method: 'POST',
    body: defaultBody({
      query: 'RELIANCE',
      exchange: 'NSE',
    }),
  },
  {
    name: 'Symbol Info',
    path: '/api/v1/symbol',
    method: 'POST',
    body: defaultBody({
      exchange: 'NSE',
      symbol: 'RELIANCE',
    }),
  },
  {
    name: 'Expiry Dates',
    path: '/api/v1/expiry',
    method: 'POST',
    body: defaultBody({
      exchange: 'NFO',
      symbol: 'NIFTY',
    }),
  },
  {
    name: 'Intervals',
    path: '/api/v1/intervals',
    method: 'POST',
    body: defaultBody(),
  },
  {
    name: 'Synthetic Future',
    path: '/api/v1/syntheticfuture',
    method: 'POST',
    body: defaultBody({
      exchange: 'NFO',
      symbol: 'NIFTY',
      expiry: '25JAN',
    }),
  },
]

// Options endpoints
const optionsEndpoints: PlaygroundEndpoint[] = [
  {
    name: 'Option Chain',
    path: '/api/v1/optionchain',
    method: 'POST',
    body: defaultBody({
      exchange: 'NFO',
      symbol: 'NIFTY',
      expiry: '25JAN',
    }),
  },
  {
    name: 'Option Symbol',
    path: '/api/v1/optionsymbol',
    method: 'POST',
    body: defaultBody({
      exchange: 'NFO',
      symbol: 'NIFTY',
      expiry: '25JAN',
      optiontype: 'CE',
      strikeprice: '22000',
    }),
  },
  {
    name: 'Option Greeks',
    path: '/api/v1/optiongreeks',
    method: 'POST',
    body: defaultBody({
      exchange: 'NFO',
      symbol: 'NIFTY25JAN22000CE',
      spot: '22000',
      interest: '10',
    }),
  },
  {
    name: 'Multi Option Greeks',
    path: '/api/v1/multioptiongreeks',
    method: 'POST',
    body: defaultBody({
      options: [
        { exchange: 'NFO', symbol: 'NIFTY25JAN22000CE', spot: '22000', interest: '10' },
        { exchange: 'NFO', symbol: 'NIFTY25JAN22000PE', spot: '22000', interest: '10' },
      ],
    }),
  },
  {
    name: 'Options Order',
    path: '/api/v1/optionsorder',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      exchange: 'NFO',
      symbol: 'NIFTY',
      expiry: '25JAN',
      optiontype: 'CE',
      strikeprice: '22000',
      action: 'BUY',
      product: 'MIS',
      pricetype: 'MARKET',
      quantity: '75',
    }),
  },
  {
    name: 'Options Multi Order',
    path: '/api/v1/optionsmultiorder',
    method: 'POST',
    body: defaultBody({
      strategy: 'Test',
      legs: [
        {
          exchange: 'NFO',
          symbol: 'NIFTY',
          expiry: '25JAN',
          optiontype: 'CE',
          strikeprice: '22000',
          action: 'BUY',
          product: 'MIS',
          pricetype: 'MARKET',
          quantity: '75',
        },
        {
          exchange: 'NFO',
          symbol: 'NIFTY',
          expiry: '25JAN',
          optiontype: 'PE',
          strikeprice: '22000',
          action: 'BUY',
          product: 'MIS',
          pricetype: 'MARKET',
          quantity: '75',
        },
      ],
    }),
  },
]

// Analyzer endpoints
const analyzerEndpoints: PlaygroundEndpoint[] = [
  {
    name: 'Analyzer Status',
    path: '/api/v1/analyzer',
    method: 'POST',
    body: defaultBody(),
  },
  {
    name: 'Analyzer Toggle',
    path: '/api/v1/analyzer/toggle',
    method: 'POST',
    body: defaultBody(),
  },
]

// Utility endpoints
const utilityEndpoints: PlaygroundEndpoint[] = [
  {
    name: 'Ping',
    path: '/api/v1/ping',
    method: 'GET',
    params: {},
  },
  {
    name: 'Instruments',
    path: '/api/v1/instruments',
    method: 'GET',
    params: {},
  },
]

// Combined endpoints by category
export const playgroundEndpoints: PlaygroundEndpointsByCategory = {
  Orders: orderEndpoints,
  Account: accountEndpoints,
  'Market Data': dataEndpoints,
  Options: optionsEndpoints,
  Analyzer: analyzerEndpoints,
  Utilities: utilityEndpoints,
}

export default playgroundEndpoints
