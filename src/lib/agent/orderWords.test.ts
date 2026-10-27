import { describe, expect, it } from 'vitest'
import { describeApproval, rupees } from './orderWords'

const place = (args: Record<string, unknown>) =>
  describeApproval({ tool_name: 'place_order', args }).sentence

describe('describeApproval', () => {
  it('states a market order the way a trader reads it back', () => {
    expect(
      place({ symbol: 'SBIN', exchange: 'NSE', action: 'BUY', quantity: 10, product: 'MIS' })
    ).toBe('Buy 10 SBIN on NSE, intraday (MIS), at market.')
  })

  it('names the price of a limit, stop-loss and stop-loss market order', () => {
    const base = {
      symbol: 'RELIANCE',
      exchange: 'NSE',
      action: 's',
      quantity: 5,
      product: 'delivery',
    }
    expect(place({ ...base, price_type: 'LIMIT', price: 1450.5 })).toBe(
      'Sell 5 RELIANCE on NSE, delivery (CNC), at a limit price of 1,450.50.'
    )
    expect(place({ ...base, price_type: 'stop loss', price: 1440, trigger_price: 1445 })).toBe(
      'Sell 5 RELIANCE on NSE, delivery (CNC), as a stop-loss limit order, triggered at 1,445.00, limit price 1,440.00.'
    )
    expect(place({ ...base, price_type: 'SL M', trigger_price: 1445 })).toBe(
      'Sell 5 RELIANCE on NSE, delivery (CNC), as a stop-loss market order, triggered at 1,445.00.'
    )
  })

  it('reads an exchange written into the symbol as the server does', () => {
    expect(
      place({
        symbol: 'NFO:NIFTY28MAR2420800CE',
        exchange: '',
        action: 'BUY',
        quantity: 75,
        product: 'NRML',
      })
    ).toBe('Buy 75 NIFTY28MAR2420800CE on NFO, carry forward (NRML), at market.')
  })

  it('words every order tool, the account-wide ones included', () => {
    const say = (tool_name: string, args: Record<string, unknown> = {}) =>
      describeApproval({ tool_name, args }).sentence
    expect(
      say('place_smart_order', {
        symbol: 'INFY',
        exchange: 'NSE',
        product: 'MIS',
        position_size: -75,
      })
    ).toBe('Bring the INFY position on NSE, intraday (MIS), to short 75, at market.')
    expect(
      say('place_smart_order', {
        symbol: 'INFY',
        exchange: 'NSE',
        product: 'MIS',
        position_size: 0,
      })
    ).toContain('to flat')
    expect(
      say('modify_order', {
        order_id: 240307000616990,
        symbol: 'SBIN',
        exchange: 'NSE',
        action: 'BUY',
        quantity: 10,
        product: 'MIS',
        price_type: 'LIMIT',
        price: 1455,
      })
    ).toBe(
      'Change order 240307000616990 to: buy 10 SBIN on NSE, intraday (MIS), at a limit price of 1,455.00.'
    )
    expect(say('cancel_order', { order_id: 'A1' })).toBe('Cancel order A1.')
    expect(say('close_position', { symbol: 'SBIN', exchange: 'NSE', product: 'MIS' })).toBe(
      'Close the SBIN position on NSE, intraday (MIS), at market.'
    )
    expect(say('cancel_all_orders')).toMatch(/^Cancel every open order on the account/)
    expect(say('close_all_positions')).toMatch(
      /^Close every open position on the account at market/
    )
  })

  it('never builds an order sentence from a call whose details did not arrive', () => {
    const words = describeApproval({
      tool_name: 'place_order',
      args: { action: 'BUY', quantity: 10 },
      missing: ['exchange', 'symbol'],
    })
    expect(words.sentence).toBe('Place order.')
    expect(words.details).toEqual([
      { name: 'action', label: 'Side', value: 'BUY' },
      { name: 'quantity', label: 'Quantity', value: '10' },
    ])
  })

  it('lists the arguments of a tool it has no sentence for', () => {
    const words = describeApproval({
      tool_name: 'save_openscript',
      args: { name: 'ema cross', code: '' },
    })
    expect(words.sentence).toBe('Save openscript.')
    expect(words.details).toEqual([{ name: 'name', label: 'Name', value: 'ema cross' }])
  })

  it('prints prices with Indian grouping and two decimals', () => {
    expect(rupees(150000)).toBe('1,50,000.00')
    expect(rupees(0.05)).toBe('0.05')
  })
})
