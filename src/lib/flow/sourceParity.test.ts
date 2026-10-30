/**
 * The web's Python tests that read the Flow frontend's source (parity X-025,
 * X-058, X-060, X-075, X-142, X-224, X-255..X-262), ported to vitest so they
 * guard the desktop's copy of the same files. Where a fact can be read from
 * the running module (DEFAULT_NODE_DATA, INDEX_SYMBOLS) it is; where the web
 * test pins source text (types, a component's wording) the source is read.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { nodeTypes } from '@/components/flow/nodes'
import { DEFAULT_NODE_DATA, INDEX_SYMBOLS, NODE_DEFINITIONS } from './constants'

const SRC = join(process.cwd(), 'src')
const source = (path: string) => readFileSync(join(SRC, path), 'utf8')
const defaults = DEFAULT_NODE_DATA as Record<string, Record<string, unknown>>
const registered = Object.keys(nodeTypes)

/** The body of `export interface <name> { ... }`, braces balanced. */
function interfaceBody(text: string, name: string): string {
  const start = text.indexOf(`export interface ${name} {`)
  if (start < 0) throw new Error(`no interface ${name}`)
  let depth = 0
  for (let i = text.indexOf('{', start); i < text.length; i++) {
    if (text[i] === '{') depth++
    if (text[i] === '}' && --depth === 0) return text.slice(start, i + 1)
  }
  throw new Error(`unbalanced interface ${name}`)
}

describe('the config panel builds custom legs itself (X-025)', () => {
  it('has the leg editor, not the old "via API" placeholder', () => {
    const panel = source('components/flow/panels/ConfigPanel.tsx')
    expect(panel).not.toContain('Configure custom legs via API')
    expect(panel).toContain('CustomLegsFields')
  })
})

describe('the editor derives the default product (X-058, X-060)', () => {
  it.each([
    'components/flow/panels/ConfigPanel.tsx',
    'components/flow/nodes/PlaceOrderNode.tsx',
    'components/flow/nodes/SmartOrderNode.tsx',
    'components/flow/nodes/OpenPositionNode.tsx',
  ])('%s uses the shared rule and hardcodes no MIS', (path) => {
    const text = source(path)
    expect(text).toContain('defaultProductForExchange')
    expect(text).not.toContain("product || 'MIS'")
    expect(text).not.toContain("nodeData.product as string) || 'MIS'")
  })
})

describe('node defaults (X-075, X-224, X-255, X-256, X-257)', () => {
  it('both options nodes default to NRML (X-075)', () => {
    expect(defaults.optionsOrder.product).toBe('NRML')
    expect(defaults.optionsMultiOrder.product).toBe('NRML')
  })

  it('modify order carries only the order id, nothing it would send as an override (X-224)', () => {
    expect(defaults.modifyOrder).toHaveProperty('orderId')
    for (const key of ['symbol', 'exchange', 'action', 'product', 'priceType']) {
      expect(defaults.modifyOrder).not.toHaveProperty(key)
    }
  })

  it('every registered node has defaults (X-255)', () => {
    for (const type of registered) expect(defaults[type], type).toBeDefined()
  })

  it.each(['smartOrder', 'basketOrder', 'splitOrder'])(
    '%s defaults carry numeric price and trigger price (X-256)',
    (type) => {
      expect(defaults[type].price).toBe(0)
      expect(defaults[type].triggerPrice).toBe(0)
    }
  )

  it('no Telegram username and no per-leg expiryDate survive (X-257)', () => {
    expect(defaults.telegramAlert).not.toHaveProperty('username')
    const types = source('types/flow.ts')
    expect(interfaceBody(types, 'TelegramAlertNodeData')).not.toContain('username')
    const multi = interfaceBody(types, 'OptionsMultiOrderNodeData')
    const legs = multi.slice(multi.indexOf('legs: Array<{'), multi.indexOf('}>') + 2)
    expect(legs).toContain('optionType')
    expect(legs).not.toContain('expiryDate')
  })
})

describe('order node types accept every price type the server does (X-258..X-260)', () => {
  const types = source('types/flow.ts')
  const ALL = "'MARKET' | 'LIMIT' | 'SL' | 'SL-M'"

  it.each(['OptionsOrderNodeData', 'OptionsMultiOrderNodeData', 'SplitOrderNodeData'])(
    '%s',
    (name) => {
      const body = interfaceBody(types, name)
      expect(body).toContain(`priceType: ${ALL}`)
      expect(body).toContain('price?: number')
      expect(body).toContain('triggerPrice?: number')
    }
  )

  it('BasketOrderNodeData', () => {
    const body = interfaceBody(types, 'BasketOrderNodeData')
    expect(body).toContain(`priceType?: ${ALL}`)
    expect(body).toContain('price?: number')
    expect(body).toContain('triggerPrice?: number')
  })

  it('custom legs carry their own product and price contract (X-260)', () => {
    const multi = interfaceBody(types, 'OptionsMultiOrderNodeData')
    const legs = multi.slice(multi.indexOf('legs: Array<{'), multi.indexOf('}>') + 2)
    expect(legs).toContain("product?: 'MIS' | 'NRML'")
    expect(legs).toContain(`priceType?: ${ALL}`)
    expect(legs).toContain('price?: number')
    expect(legs).toContain('triggerPrice?: number')
    // The top-level prices sit after the legs array.
    expect(multi.lastIndexOf('price?: number')).toBeGreaterThan(multi.indexOf('}>'))
  })
})

describe('the palette and config titles cover every node (X-261, X-262)', () => {
  it('offers every registered node in the palette (X-261)', () => {
    const palette = new Set<string>(
      Object.values(NODE_DEFINITIONS)
        .flat()
        .map((d) => d.type)
    )
    for (const type of registered) expect(palette.has(type), type).toBe(true)
  })

  it('gives every registered node a config panel title (X-262)', () => {
    const panel = source('components/flow/panels/ConfigPanel.tsx')
    const start = panel.indexOf('const NODE_TITLES: Record<string, string> = {')
    const block = panel.slice(start, panel.indexOf('\n}', start))
    const titled = new Set([...block.matchAll(/^\s+(\w+): '/gm)].map((m) => m[1]))
    for (const type of registered) expect(titled.has(type), type).toBe(true)
  })
})

describe('index underlyings (X-142)', () => {
  it('keeps the NFO and BFO underlyings as the web has them', () => {
    const on = (exchange: string) =>
      INDEX_SYMBOLS.filter((s) => s.exchange === exchange)
        .map((s) => s.value)
        .sort()
    expect(on('BFO')).toEqual(['BANKEX', 'SENSEX', 'SENSEX50'])
    expect(on('NFO')).toEqual(['BANKNIFTY', 'FINNIFTY', 'MIDCPNIFTY', 'NIFTY', 'NIFTYNXT50'])
  })
})
