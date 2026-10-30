/**
 * The Flow node data types (parity F-230..F-236, F-259, X-259). They are
 * type-only, so no runtime test can catch a drift and the app's `tsc -b`
 * does not compile test files; this reads src/types/flow.ts the way the
 * web's own tests read the editor's sources, and pins every exported
 * interface's members and every exported type alias against
 * flow.surface.json (the surface as carried from the web at 96e5927e5). A
 * change to the workflow format's types is then a deliberate change to that
 * file too. To accept one, write the new surface with
 * FLOW_SURFACE_WRITE=1 npx vitest run src/types/flow.surface.test.ts
 * and review the diff.
 */

import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, expectTypeOf, it } from 'vitest'
import type {
  Exchange,
  NodeCategory,
  OptionType,
  OrderAction,
  PriceType,
  ProductType,
  ScheduleType,
} from '@/lib/flow/constants'

const SOURCE = readFileSync(join(process.cwd(), 'src/types/flow.ts'), 'utf8')
  // Comments are prose, not the type surface.
  .replace(/\/\*[\s\S]*?\*\//g, '')
  .replace(/\/\/[^\n]*/g, '')

/** The text between an opening brace at `open` and its matching close. */
function block(text: string, open: number): string {
  let depth = 0
  for (let i = open; i < text.length; i++) {
    if (text[i] === '{') depth++
    if (text[i] === '}' && --depth === 0) return text.slice(open + 1, i)
  }
  throw new Error('unbalanced braces in src/types/flow.ts')
}

const squash = (s: string) => s.replace(/\s+/g, ' ').trim()

function interfaces(): Record<string, string> {
  const out: Record<string, string> = {}
  for (const m of SOURCE.matchAll(/export interface (\w+)(?: extends [^{]+)? \{/g)) {
    out[m[1]] = squash(block(SOURCE, (m.index ?? 0) + m[0].length - 1))
  }
  return out
}

function aliases(): Record<string, string> {
  const out: Record<string, string> = {}
  for (const m of SOURCE.matchAll(/export type (\w+)(<[^=]*>)? =([\s\S]*?)(?=\nexport |\n*$)/g)) {
    out[m[1]] = squash(m[3])
  }
  return out
}

const FIXTURE = join(process.cwd(), 'src/types/flow.surface.json')
if (process.env.FLOW_SURFACE_WRITE === '1') {
  writeFileSync(
    FIXTURE,
    `${JSON.stringify({ interfaces: interfaces(), aliases: aliases() }, null, 2)}\n`
  )
}
const pinned = JSON.parse(readFileSync(FIXTURE, 'utf8')) as {
  interfaces: Record<string, string>
  aliases: Record<string, string>
}

describe('the Flow node data types', () => {
  it('keep every interface member (F-230..F-235)', () => {
    const found = interfaces()
    expect(Object.keys(found).length).toBeGreaterThan(60)
    expect(found).toEqual(pinned.interfaces)
  })

  it('keep every type alias and union (F-236, F-259)', () => {
    expect(aliases()).toEqual(pinned.aliases)
  })

  it('give the options multi-order node its own top-level prices, after the legs (X-259)', () => {
    const multi = interfaces().OptionsMultiOrderNodeData
    const afterLegs = multi.slice(multi.indexOf('}>') + 2)
    expect(afterLegs).toContain('price?: number')
    expect(afterLegs).toContain('triggerPrice?: number')
  })

  it('derive NodeType from the canvas registry, as the web does (F-236)', () => {
    expect(aliases().NodeType).toBe('keyof typeof nodeTypes')
  })
})

/**
 * The option-list aliases src/lib/flow/constants.ts derives from its own
 * lists (F-259). The text pins how each is derived; the type assertions pin
 * what it resolves to, checked by CI's `tsc -p tsconfig.test.json` (vitest
 * itself does not check types).
 */
describe('the constants type aliases (F-259)', () => {
  const constants = readFileSync(join(process.cwd(), 'src/lib/flow/constants.ts'), 'utf8')
  const declared = Object.fromEntries(
    [...constants.matchAll(/^export type (\w+) = (.+)$/gm)].map((m) => [m[1], m[2].trim()])
  )

  it('derives each alias from its list, as the web does', () => {
    expect(declared).toEqual({
      IndicatorParamType: "'int' | 'float' | 'bool' | 'string'",
      Exchange: "(typeof EXCHANGES)[number]['value']",
      ProductType: "(typeof PRODUCT_TYPES)[number]['value']",
      PriceType: "(typeof PRICE_TYPES)[number]['value']",
      OrderAction: "(typeof ORDER_ACTIONS)[number]['value']",
      OptionType: "(typeof OPTION_TYPES)[number]['value']",
      ScheduleType: "(typeof SCHEDULE_TYPES)[number]['value']",
      NodeCategory: '(typeof NODE_CATEGORIES)[keyof typeof NODE_CATEGORIES]',
    })
  })

  it('resolves each alias to the values its list offers', () => {
    expectTypeOf<Exchange>().toEqualTypeOf<
      | 'NSE'
      | 'BSE'
      | 'NFO'
      | 'BFO'
      | 'CDS'
      | 'BCD'
      | 'MCX'
      | 'NCDEX'
      | 'NCO'
      | 'NSE_INDEX'
      | 'BSE_INDEX'
      | 'MCX_INDEX'
      | 'GLOBAL_INDEX'
      | 'CRYPTO'
    >()
    expectTypeOf<ProductType>().toEqualTypeOf<'MIS' | 'CNC' | 'NRML'>()
    expectTypeOf<PriceType>().toEqualTypeOf<'MARKET' | 'LIMIT' | 'SL' | 'SL-M'>()
    expectTypeOf<OrderAction>().toEqualTypeOf<'BUY' | 'SELL'>()
    expectTypeOf<OptionType>().toEqualTypeOf<'CE' | 'PE'>()
    expectTypeOf<ScheduleType>().toEqualTypeOf<'once' | 'daily' | 'weekly' | 'interval'>()
    expectTypeOf<NodeCategory>().toEqualTypeOf<
      'triggers' | 'actions' | 'conditions' | 'data' | 'utilities'
    >()
  })
})
