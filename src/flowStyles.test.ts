/**
 * The Flow editor's styles in src/index.css (parity D-53, F-292, F-293,
 * F-405, F-672, F-673). The block is the web's, restored, with the web's
 * hsl(var(--x)) on this app's oklch tokens rewritten (those declarations
 * were dropped by the browser) and the node and buy/sell colours mapped into
 * the theme. tokens.test.ts scans .ts/.tsx only, so the stylesheet is read
 * here.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

const CSS = readFileSync(join(process.cwd(), 'src/index.css'), 'utf8')
const withoutComments = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, '')

const FLOW = withoutComments(
  CSS.slice(CSS.indexOf('FLOW EDITOR STYLES'), CSS.indexOf('CUSTOM SCROLLBAR STYLES'))
)
const THEME = CSS.slice(
  CSS.indexOf('@theme inline {'),
  CSS.indexOf('}', CSS.indexOf('@theme inline {'))
)

/** The declarations of the first rule whose selector is exactly `selector`. */
function rule(selector: string): string {
  const at = FLOW.indexOf(`${selector} {`)
  expect(at, selector).toBeGreaterThan(-1)
  return FLOW.slice(at, FLOW.indexOf('}', at))
}

describe('the Flow editor block is restored (D-53, F-292, F-405, F-672)', () => {
  it.each([
    '.react-flow__background',
    '.react-flow__node',
    '.react-flow__handle',
    '.react-flow__handle:hover',
    '.react-flow__handle.valid',
    '.react-flow__edge-path',
    '.react-flow__connection-line',
    '.react-flow__controls',
    '.react-flow__controls-button',
    '.react-flow__minimap',
    '.react-flow__minimap-mask',
    '.react-flow__minimap-node',
    '.workflow-node',
    '.workflow-node:hover',
    '.workflow-node.selected',
    '.node-trigger',
    '.node-start',
    '.node-action',
    '.node-condition',
    '.node-data',
    '.node-utility',
    '.node-icon',
    '.flow-status-dot',
    '.flow-status-dot.running',
    '.mono-data',
    '.badge-buy',
    '.badge-sell',
    '.bg-surface-2',
  ])('styles %s', (selector) => {
    expect(rule(selector).length).toBeGreaterThan(selector.length + 2)
  })

  it('defines the node colour channels for light and dark, and the edge and status animations', () => {
    for (const token of ['trigger', 'action', 'condition', 'data', 'utility']) {
      expect(FLOW.match(new RegExp(`--node-${token}: \\d+ \\d+% \\d+%;`, 'g')), token).toHaveLength(
        2
      )
    }
    expect(FLOW).toContain('@keyframes dashdraw')
    expect(FLOW).toContain('@keyframes flow-pulse')
  })
})

describe('every colour in the block paints (F-293)', () => {
  it('wraps none of the app oklch tokens in hsl()', () => {
    const wrapped = [...FLOW.matchAll(/hsl\(var\(--([a-z-]+)\)/g)].map((m) => m[1])
    expect(wrapped.filter((token) => !token.startsWith('node-'))).toEqual([])
  })

  it('reads the theme names instead', () => {
    expect(rule('.react-flow__background')).toContain('var(--color-background)')
    expect(rule('.react-flow__handle')).toContain('var(--color-foreground)')
    expect(rule('.react-flow__minimap-mask')).toContain(
      'color-mix(in oklab, var(--color-background) 90%, transparent)'
    )
  })
})

describe('the node and buy/sell colours are theme colours (F-673)', () => {
  it.each([
    ['--color-buy', 'hsl(var(--buy))'],
    ['--color-sell', 'hsl(var(--sell))'],
    ['--color-node-trigger', 'hsl(var(--node-trigger))'],
    ['--color-node-action', 'hsl(var(--node-action))'],
    ['--color-node-condition', 'hsl(var(--node-condition))'],
    ['--color-node-data', 'hsl(var(--node-data))'],
    ['--color-node-utility', 'hsl(var(--node-utility))'],
  ])('%s maps to %s', (name, value) => {
    expect(THEME).toContain(`${name}: ${value};`)
  })

  it('keeps the BUY/SELL badge text readable in the light theme and the web shades in dark', () => {
    expect(rule('.badge-buy')).toContain('text-[hsl(142_76%_24%)]')
    expect(rule('.badge-sell')).toContain('text-[hsl(0_84%_38%)]')
    expect(FLOW).toContain(':is(.dark, .analyzer) .badge-buy {\n  @apply text-[hsl(142_76%_46%)];')
    expect(FLOW).toContain(':is(.dark, .analyzer) .badge-sell {\n  @apply text-[hsl(0_84%_70%)];')
  })
})
