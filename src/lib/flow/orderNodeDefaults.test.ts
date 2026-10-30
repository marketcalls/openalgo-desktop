/**
 * What a freshly dropped order node carries (the web's DEFAULT_NODE_DATA,
 * frontend/src/lib/flow/constants.ts 1235-1356). The executor reads these
 * values as they are stored: a default that drifts places a different order
 * from an untouched node (parity N-placeOrder-01, N-smartOrder-01,
 * N-optionsOrder-01, N-optionsMultiOrder-02, N-cancelOrder-01,
 * N-cancelAllOrders-01, N-closePositions-01, N-modifyOrder-01,
 * N-basketOrder-01, N-splitOrder-01).
 */

import { describe, expect, it } from 'vitest'
import { DEFAULT_NODE_DATA } from './constants'

const defaults = DEFAULT_NODE_DATA as Record<string, Record<string, unknown>>

describe('the order nodes start from the web defaults', () => {
  it.each([
    [
      'placeOrder',
      {
        symbol: '',
        exchange: 'NSE',
        action: 'BUY',
        quantity: 1,
        priceType: 'MARKET',
        price: 0,
        triggerPrice: 0,
      },
    ],
    [
      'smartOrder',
      {
        symbol: '',
        exchange: 'NSE',
        action: 'BUY',
        quantity: 1,
        positionSize: 0,
        priceType: 'MARKET',
        price: 0,
        triggerPrice: 0,
      },
    ],
    [
      'optionsOrder',
      {
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
      },
    ],
    [
      'optionsMultiOrder',
      {
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
      },
    ],
    ['cancelOrder', { orderId: '' }],
    ['cancelAllOrders', {}],
    ['closePositions', { symbol: '', exchange: 'NSE' }],
    ['modifyOrder', { orderId: '' }],
    ['basketOrder', { orders: '', priceType: 'MARKET', price: 0, triggerPrice: 0 }],
    [
      'splitOrder',
      {
        symbol: '',
        exchange: 'NSE',
        action: 'BUY',
        quantity: 100,
        splitSize: 50,
        priceType: 'MARKET',
        price: 0,
        triggerPrice: 0,
      },
    ],
  ])('%s', (type, expected) => {
    // toStrictEqual: no extra key either (an extra exchange or action on
    // modifyOrder would be sent as an override; an extra product would stop
    // the node following its exchange).
    expect(defaults[type]).toStrictEqual(expected)
  })
})
