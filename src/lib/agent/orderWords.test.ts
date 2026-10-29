import { describe, expect, it } from 'vitest'
import { describeApproval, describeSmartPreview, rupees } from './orderWords'

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

describe('describeSmartPreview', () => {
  it('states the difference a smart order sends, not its target', () => {
    expect(
      describeSmartPreview({ held: 500, target: -500, action: 'SELL', quantity: 1000 }, false)
    ).toBe('Held now: long 500. This sends Sell 1,000 to reach short 500.')
    expect(
      describeSmartPreview({ held: 100, target: 150, action: 'BUY', quantity: 50 }, false)
    ).toBe('Held now: long 100. This sends Buy 50 to reach long 150.')
    expect(describeSmartPreview({ held: -75, target: 0, action: 'BUY', quantity: 75 }, false)).toBe(
      'Held now: short 75. This sends Buy 75 to reach flat.'
    )
    expect(describeSmartPreview({ held: 100, target: 100, action: null, quantity: 0 }, false)).toBe(
      'The position is already long 100: nothing will be placed.'
    )
  })

  it('never guesses when the position cannot be read', () => {
    expect(describeSmartPreview(undefined, false)).toBe('Reading the position held now.')
    expect(describeSmartPreview({ held: null, target: -500 }, false)).toMatch(/could not be read/)
    expect(describeSmartPreview(undefined, true)).toMatch(/could not be read/)
    expect(
      describeSmartPreview(
        { held: null, error: "The 'position_size' argument is invalid: x" },
        false
      )
    ).toBe("The 'position_size' argument is invalid: x")
  })
})

describe('describeApproval for save_flow (parity A-25)', () => {
  const graph = {
    name: 'ORB',
    nodes: [
      { id: 'n1', type: 'start', data: {} },
      { id: 'n2', type: 'placeOrder', data: {} },
    ],
    edges: [{ id: 'e1', source: 'n1', target: 'n2' }],
  }

  it('says what is saved, under the name the tool stores, and that it does not run', () => {
    const words = describeApproval({
      tool_name: 'save_flow',
      args: { name: 'Opening range', workflow_json: JSON.stringify(graph) },
    })
    expect(words.sentence).toBe(
      'Save the workflow "Opening range (imported)" to Flow. It is saved inactive and does not run until you activate it.'
    )
    expect(words.details).toContainEqual({
      name: 'workflow_json',
      label: 'Workflow',
      value: '2 nodes, 1 connection',
    })
  })

  it('lists a very large workflow by its size, never its text', () => {
    const big = {
      nodes: Array.from({ length: 4000 }, (_, i) => ({
        id: `n${i}`,
        type: 'log',
        data: { message: 'x'.repeat(100) },
      })),
      edges: [],
    }
    const json = JSON.stringify(big)
    expect(json.length).toBeGreaterThan(400_000)
    const words = describeApproval({
      tool_name: 'save_flow',
      args: { name: 'Big', workflow_json: json },
    })
    const detail = words.details.find((d) => d.name === 'workflow_json')
    expect(detail?.value).toBe('4000 nodes, 0 connections')
  })

  it('reads an object argument and an unreadable one', () => {
    const asObject = describeApproval({
      tool_name: 'save_flow',
      args: { name: 'A', workflow_json: graph },
    })
    expect(asObject.details.find((d) => d.name === 'workflow_json')?.value).toBe(
      '2 nodes, 1 connection'
    )
    const broken = describeApproval({
      tool_name: 'save_flow',
      args: { name: 'A', workflow_json: '{' },
    })
    expect(broken.details.find((d) => d.name === 'workflow_json')?.value).toBe(
      'not readable as a workflow'
    )
  })

  it('falls back to the tool name with no workflow name', () => {
    expect(describeApproval({ tool_name: 'save_flow', args: {} }).sentence).toBe('Save flow.')
  })
})
