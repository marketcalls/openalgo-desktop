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

/**
 * The declarations of the first rule that lists `selector` whole, alone or in
 * a selector list (`.a,\n.b {` lists both `.a` and `.b`).
 */
function rule(selector: string): string {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  const at = FLOW.search(new RegExp(`^${escaped}\\s*[,{]`, 'm'))
  expect(at, selector).toBeGreaterThan(-1)
  const open = FLOW.indexOf('{', at)
  return FLOW.slice(open + 1, FLOW.indexOf('}', open))
}

describe('the Flow editor block is restored (D-53, F-292, F-405, F-672)', () => {
  it.each([
    '.react-flow__background',
    '.react-flow__node',
    '.react-flow__handle',
    '.react-flow__handle:hover',
    '.react-flow__handle.valid',
    '.react-flow__handle.connectingto',
    '.react-flow__handle.connectingfrom',
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
    '.node-trigger .node-icon',
    '.node-start .node-icon',
    '.node-action .node-icon',
    '.node-condition .node-icon',
    '.node-data .node-icon',
    '.node-utility .node-icon',
    '.flow-status-dot',
    '.flow-status-dot.running',
    '.mono-data',
    '.badge-buy',
    '.badge-sell',
    '.bg-surface-2',
  ])('styles %s', (selector) => {
    expect(rule(selector)).toMatch(/[\w-]+:|@apply /)
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

/**
 * F-672. The run-state dots carry meaning (active green, running amber,
 * inactive greyed, error red), so their colours are pinned. The web paints
 * inactive and error with hsl(var(--muted-foreground) / 0.5) and
 * hsl(var(--destructive)), which this app's oklch tokens turn into dropped
 * declarations; the desktop reads the theme names instead (intended, F-293).
 * The 9px subtitle and the node-icon size are styling (decision 35).
 */
describe('the run-state dots keep their colours (F-672)', () => {
  it.each([
    ['.flow-status-dot.active', 'background-color: hsl(var(--node-data));'],
    ['.flow-status-dot.running', 'background-color: hsl(var(--node-trigger));'],
    [
      '.flow-status-dot.inactive',
      'background-color: color-mix(in oklab, var(--color-muted-foreground) 50%, transparent);',
    ],
    ['.flow-status-dot.error', 'background-color: var(--color-destructive);'],
  ])('%s paints %s', (selector, declaration) => {
    expect(rule(selector)).toContain(declaration)
  })

  it('pulses only the running dot', () => {
    expect(rule('.flow-status-dot.running')).toContain(
      'animation: flow-pulse 2s ease-in-out infinite;'
    )
    for (const state of ['active', 'inactive', 'error']) {
      expect(rule(`.flow-status-dot.${state}`), state).not.toContain('animation')
    }
  })
})

/**
 * F-672. The --node-* channels are this block's own HSL triplets, not the
 * app's oklch tokens, so they carry over from the web unchanged. These are the
 * web's values at 96e5927e5 (frontend/src/index.css, :root and .dark); the
 * desktop must equal them.
 */
const WEB_NODE_CHANNELS: Array<[block: string, token: string, value: string]> = [
  [':root', 'trigger', '38 92% 50%'],
  [':root', 'action', '217 91% 60%'],
  [':root', 'condition', '270 60% 60%'],
  [':root', 'data', '142 76% 36%'],
  [':root', 'utility', '200 70% 50%'],
  ['.dark', 'trigger', '38 92% 55%'],
  ['.dark', 'action', '217 91% 65%'],
  ['.dark', 'condition', '270 60% 65%'],
  ['.dark', 'data', '142 76% 46%'],
  ['.dark', 'utility', '200 70% 55%'],
]

describe("the node colour channels equal the web's (F-672)", () => {
  it.each(WEB_NODE_CHANNELS)('%s sets --node-%s to %s', (block, token, value) => {
    expect(rule(block)).toContain(`--node-${token}: ${value};`)
  })
})
