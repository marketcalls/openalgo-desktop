/**
 * The sentences a trader reads under a custom option leg or a margin basket
 * leg, verbatim, and the limits behind them (parity F-271, F-273, F-274,
 * F-281, F-284, V-155, V-156). The carried web tests check which field is
 * flagged; these pin what it says.
 */

import { describe, expect, it } from 'vitest'
import {
  type CustomLeg,
  EMPTY_CUSTOM_LEG,
  NEEDS_PRICE as LEG_NEEDS_PRICE,
  NEEDS_TRIGGER as LEG_NEEDS_TRIGGER,
  MAX_CUSTOM_LEGS,
  validateCustomLeg,
  validateCustomLegs,
} from './customLegs'
import {
  EMPTY_LEG,
  LOT_TRADED_EXCHANGES,
  MAX_LEGS,
  type MarginLeg,
  NEEDS_PRICE,
  NEEDS_TRIGGER,
  validateBasket,
  validateLeg,
} from './marginPositions'

const leg = (patch: Partial<CustomLeg>): CustomLeg => ({ ...EMPTY_CUSTOM_LEG, ...patch })
const position = (patch: Partial<MarginLeg>): MarginLeg => ({
  ...EMPTY_LEG,
  symbol: 'SBIN',
  extra: {},
  ...patch,
})

describe('custom leg limits (F-271)', () => {
  it('caps a basket at 10 legs and starts a leg at ATM CE BUY 1, inheriting the rest', () => {
    expect(MAX_CUSTOM_LEGS).toBe(10)
    expect(EMPTY_CUSTOM_LEG).toEqual({
      strikeMode: 'OFFSET',
      offset: 'ATM',
      strike: '',
      expiryMode: 'INHERIT',
      expiryType: 'current_week',
      expiry: '',
      optionType: 'CE',
      action: 'BUY',
      quantity: '1',
      product: '',
      priceType: '',
      price: '',
      triggerPrice: '',
      splitSize: '',
    })
    expect([...LEG_NEEDS_PRICE].sort()).toEqual(['LIMIT', 'SL'])
    expect([...LEG_NEEDS_TRIGGER].sort()).toEqual(['SL', 'SL-M'])
  })
})

describe('custom leg sentences (F-273, V-155)', () => {
  it.each([
    [{ strikeMode: 'STRIKE' as const, strike: 'abc' }, 'strike', 'Strike must be a number'],
    [{ strikeMode: 'STRIKE' as const, strike: '-5' }, 'strike', 'Strike must be at least 0'],
    [{ strikeMode: 'STRIKE' as const, strike: '0' }, 'strike', 'Strike must be greater than 0'],
    [{ offset: '' }, 'offset', 'Offset is required'],
    [{ offset: 'OTM99' }, 'offset', 'Use ATM, ITM1-ITM50 or OTM1-OTM50'],
    [{ expiryMode: 'DATE' as const, expiry: '' }, 'expiry', 'Expiry is required'],
    [{ expiryMode: 'DATE' as const, expiry: '2025-10-28' }, 'expiry', 'Use DDMMMYY, e.g. 28OCT25'],
    [{ quantity: 'x' }, 'quantity', 'Quantity must be a number'],
    [{ quantity: '0' }, 'quantity', 'Quantity must be at least 1'],
    [{ splitSize: 'x' }, 'splitSize', 'Split size must be a number'],
    [{ splitSize: '-1' }, 'splitSize', 'Split size must be at least 0'],
    [{ priceType: 'LIMIT', price: 'x' }, 'price', 'Price must be a number'],
    [{ priceType: 'LIMIT', price: '0' }, 'price', 'LIMIT needs a price above 0'],
    [
      { priceType: 'SL', price: '10', triggerPrice: 'x' },
      'triggerPrice',
      'Trigger price must be a number',
    ],
    [
      { priceType: 'SL-M', triggerPrice: '0' },
      'triggerPrice',
      'SL-M needs a trigger price above 0',
    ],
  ])('%j reads on %s: %s', (patch, field, sentence) => {
    expect(validateCustomLeg(leg(patch), 'MARKET')[field as keyof CustomLeg]).toBe(sentence)
  })

  it("names the node's price type when the leg inherits it", () => {
    expect(validateCustomLeg(leg({ price: '0' }), 'SL').price).toBe('SL needs a price above 0')
    expect(validateCustomLeg(leg({ triggerPrice: '0', price: '5' }), 'SL').triggerPrice).toBe(
      'SL needs a trigger price above 0'
    )
  })
})

describe('custom leg basket sentences (F-274)', () => {
  it('names the cap, the empty basket and the first incomplete leg', () => {
    expect(validateCustomLegs([], 'MARKET')).toBe('Add at least one leg')
    expect(
      validateCustomLegs(
        Array.from({ length: 11 }, () => leg({})),
        'MARKET'
      )
    ).toBe('At most 10 legs')
    expect(validateCustomLegs([leg({}), leg({}), leg({ offset: '' })], 'MARKET')).toBe(
      'Leg 3 is incomplete'
    )
    expect(validateCustomLegs([leg({})], 'MARKET')).toBeNull()
  })
})

describe('margin basket limits (F-281)', () => {
  it('caps a basket at 50 positions, looks lots up on NFO and BFO, and starts an NSE MIS leg', () => {
    expect(MAX_LEGS).toBe(50)
    expect([...LOT_TRADED_EXCHANGES].sort()).toEqual(['BFO', 'NFO'])
    expect(EMPTY_LEG).toEqual({
      symbol: '',
      exchange: 'NSE',
      action: 'BUY',
      quantity: '1',
      product: 'MIS',
      pricetype: 'MARKET',
      price: '0',
      trigger_price: '0',
      extra: {},
    })
    expect([...NEEDS_PRICE].sort()).toEqual(['LIMIT', 'SL'])
    expect([...NEEDS_TRIGGER].sort()).toEqual(['SL', 'SL-M'])
  })
})

describe('margin leg sentences (F-284, V-156)', () => {
  it.each([
    [{ symbol: '' }, 'symbol', 'Symbol is required'],
    [{ exchange: 'LSE' }, 'exchange', 'Unsupported exchange'],
    [{ action: 'HOLD' }, 'action', 'Action must be BUY or SELL'],
    [{ product: 'XYZ' }, 'product', 'Unsupported product'],
    [{ pricetype: 'ICEBERG' }, 'pricetype', 'Unsupported price type'],
    [{ quantity: '1.5' }, 'quantity', 'Quantity must be a positive whole number'],
    [{ pricetype: 'LIMIT', price: '0' }, 'price', 'LIMIT needs a price above 0'],
    [
      { pricetype: 'SL', price: '10', trigger_price: '0' },
      'trigger_price',
      'SL needs a trigger price above 0',
    ],
    [
      { pricetype: 'SL-M', trigger_price: '0' },
      'trigger_price',
      'SL-M needs a trigger price above 0',
    ],
  ])('%j reads on %s: %s', (patch, field, sentence) => {
    expect(validateLeg(position(patch))[field as keyof MarginLeg]).toBe(sentence)
  })

  it('counts the positions that need attention, one or many', () => {
    expect(validateBasket([])).toEqual(['Add at least one position'])
    expect(validateBasket([position({ symbol: '' })])).toEqual(['1 position needs attention'])
    expect(validateBasket([position({ symbol: '' }), position({ quantity: '0' })])).toEqual([
      '2 positions need attention',
    ])
    expect(validateBasket(Array.from({ length: 51 }, () => position({})))).toEqual([
      'A basket can hold at most 50 positions',
    ])
  })
})
