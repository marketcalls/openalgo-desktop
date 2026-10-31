/**
 * The utility node faces and the group container, pinned as the web draws
 * them (parity F-683, F-685): delay formatting old and new, the log level tag
 * and its colour, the variable operation symbols, the truncations, the HTTP
 * method colours, and the group's resizer, colour classes, title and handles.
 * Each node is drawn on a real canvas.
 */

import { render } from '@testing-library/react'
import { type Node, type NodeResizerProps, ReactFlow, ReactFlowProvider } from '@xyflow/react'
import { beforeEach, describe, expect, it, vi } from 'vitest'

/** The props GroupNode hands xyflow's NodeResizer, which draws nothing when hidden. */
const resizer = vi.hoisted(() => ({ props: [] as NodeResizerProps[] }))

vi.mock('@xyflow/react', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@xyflow/react')>()
  const { createElement } = await import('react')
  return {
    ...actual,
    NodeResizer: (props: NodeResizerProps) => {
      resizer.props.push(props)
      return createElement(actual.NodeResizer, props)
    },
  }
})

const { nodeTypes } = await import('./index')

function face(type: string, data: Record<string, unknown> = {}, selected = false): HTMLElement {
  const node: Node = { id: `n-${type}`, type, position: { x: 0, y: 0 }, data, selected }
  const { container } = render(
    <div style={{ width: 800, height: 600 }}>
      <ReactFlowProvider>
        <ReactFlow nodes={[node]} edges={[]} nodeTypes={nodeTypes} />
      </ReactFlowProvider>
    </div>
  )
  const el = container.querySelector<HTMLElement>(`.react-flow__node[data-id="${node.id}"]`)
  if (!el) throw new Error(`${type} not drawn`)
  return el
}

function handles(el: HTMLElement) {
  return [...el.querySelectorAll<HTMLElement>('.react-flow__handle')].map((h) => ({
    id: h.dataset.handleid ?? null,
    kind: h.classList.contains('target') ? 'target' : 'source',
    position: h.dataset.handlepos,
  }))
}

/** The innermost element (of `tag`, when given) whose whole text is `text`. */
function leaf(el: HTMLElement, text: string, tag = '*'): HTMLElement {
  const found = [...el.querySelectorAll<HTMLElement>(tag)].find(
    (child) => child.children.length === 0 && child.textContent === text
  )
  if (!found) throw new Error(`no "${text}" in ${el.textContent}`)
  return found
}

function subtitle(el: HTMLElement, title: string): string | null | undefined {
  return leaf(el, title, 'div').nextElementSibling?.textContent
}

function row(el: HTMLElement, label: string): string | null | undefined {
  return leaf(el, label, 'span').nextElementSibling?.textContent
}

function has(el: HTMLElement, text: string): boolean {
  return [...el.querySelectorAll<HTMLElement>('*')].some(
    (child) => child.children.length === 0 && child.textContent === text
  )
}

beforeEach(() => {
  resizer.props = []
})

describe('F-683 the utility nodes', () => {
  it.each([
    'delay',
    'waitUntil',
    'log',
    'variable',
    'mathExpression',
    'httpRequest',
    'telegramAlert',
    'whatsappAlert',
  ])('a %s takes one wire in and gives one unnamed wire out', (type) => {
    expect(handles(face(type))).toEqual([
      { id: null, kind: 'target', position: 'top' },
      { id: null, kind: 'source', position: 'bottom' },
    ])
  })

  it.each([
    [{ delayValue: 5, delayUnit: 'seconds' }, '5s'],
    [{ delayValue: 3, delayUnit: 'minutes' }, '3m'],
    [{ delayValue: 2, delayUnit: 'hours' }, '2h'],
    [{ delayValue: 7 }, '7s'],
    [{ delayValue: 0, delayUnit: 'minutes' }, '0m'],
    [{ delayMs: 90000 }, '1m 30s'],
    [{ delayMs: 120000 }, '2m'],
    [{ delayMs: 2500 }, '2.5s'],
    [{ delayMs: 500 }, '500ms'],
    [{}, '1s'],
  ])('Delay shows %o as %s', (data, shown) => {
    const el = face('delay', data)
    expect(subtitle(el, 'Wait Duration')).toBe('Delay')
    expect(has(el, shown)).toBe(true)
  })

  it('Wait Until shows the target time, 09:30 by default, and the label when set', () => {
    const el = face('waitUntil', { targetTime: '15:10', label: 'Before close' })
    expect(subtitle(el, 'Wait Until')).toBe('Pause execution')
    expect(row(el, 'Time')).toBe('15:10')
    expect(has(el, 'Before close')).toBe(true)
    const blank = face('waitUntil')
    expect(row(blank, 'Time')).toBe('09:30')
    expect(blank.querySelector('.truncate')).toBeNull()
  })

  it.each([
    ['info', 'INFO', 'text-blue-400'],
    ['warn', 'WARN', 'text-yellow-400'],
    ['error', 'ERROR', 'text-red-400'],
    ['debug', 'INFO', 'text-muted-foreground'],
  ])('Log tags a %s entry %s in %s', (level, tag, colour) => {
    const el = face('log', { level, message: 'Entry placed' })
    expect(leaf(el, tag, 'div').classList.contains(colour)).toBe(true)
    expect(has(el, 'Entry placed')).toBe(true)
  })

  it('Log is titled "Log", with its level tag under the title', () => {
    expect(subtitle(face('log', { level: 'warn' }), 'Log')).toBe('WARN')
    expect(subtitle(face('log'), 'Log')).toBe('INFO')
  })

  it('Log shows the first 30 characters of the message, Log message by default', () => {
    const long = 'Order placed for {{symbol}} at {{ltp}} with qty {{qty}}'
    expect(has(face('log', { message: long }), `${long.slice(0, 30)}...`)).toBe(true)
    const exactly30 = 'x'.repeat(30)
    expect(has(face('log', { message: exactly30 }), exactly30)).toBe(true)
    expect(has(face('log'), 'Log message')).toBe(true)
  })

  it.each([
    ['set', 'Set Value', '='],
    ['get', 'Get Value', 'GET'],
    ['add', 'Add', '+'],
    ['subtract', 'Subtract', '-'],
    ['multiply', 'Multiply', '*'],
    ['divide', 'Divide', '/'],
    ['parse_json', 'Parse JSON', 'JSON'],
    ['stringify', 'To String', 'STR'],
    ['increment', 'Increment', '++'],
    ['decrement', 'Decrement', '--'],
    ['append', 'Append', '+='],
  ])('Variable describes %s as "%s" with the symbol %s', (operation, description, symbol) => {
    const el = face('variable', { operation, variableName: 'entry', value: 5 })
    expect(subtitle(el, 'Variable')).toBe(description)
    expect(row(el, 'Name')).toBe('entry')
    expect(row(el, symbol)).toBe('5')
    expect(has(el, 'Use: {{entry}}')).toBe(true)
  })

  it('Variable truncates the value at 20 characters and falls back to Set, var and -', () => {
    const el = face('variable', { operation: 'set', value: 'abcdefghijklmnopqrstuvwxyz' })
    expect(row(el, '=')).toBe('abcdefghijklmnopqrst')
    const json = face('variable', { operation: 'set', value: { symbol: 'SBIN', qty: 10 } })
    expect(row(json, '=')).toBe(`${JSON.stringify({ symbol: 'SBIN', qty: 10 }).slice(0, 20)}...`)
    const blank = face('variable', { operation: 'other' })
    expect(subtitle(blank, 'Variable')).toBe('Set')
    expect(row(blank, 'Name')).toBe('var')
    expect(row(blank, '=')).toBe('-')
    expect(has(blank, 'Use: {{var}}')).toBe(true)
  })

  it('Math shows an expression up to 25 characters whole, a longer one cut at 22 plus ...', () => {
    const short = '({{ltp}} - {{entry}}) * 2'
    expect(short).toHaveLength(25)
    const el = face('mathExpression', { expression: short, outputVariable: 'pnl' })
    expect(has(el, 'Math')).toBe(true)
    expect(has(el, short)).toBe(true)
    expect(has(el, 'pnl = ...')).toBe(true)
    const long = '({{ltp}} - {{entry}}) * {{qty}}'
    expect(has(face('mathExpression', { expression: long }), `${long.substring(0, 22)}...`)).toBe(
      true
    )
    expect(has(face('mathExpression', { expression: long }), 'result = ...')).toBe(true)
  })

  it('Math asks to be configured when it has no expression', () => {
    const el = face('mathExpression')
    expect(has(el, 'Configure expression')).toBe(true)
    expect(has(el, 'result = ...')).toBe(false)
  })

  it.each([
    ['GET', 'text-green-500'],
    ['POST', 'text-blue-500'],
    ['PUT', 'text-orange-500'],
    ['DELETE', 'text-red-500'],
    ['PATCH', 'text-purple-500'],
    ['HEAD', 'text-muted-foreground'],
  ])('HTTP Request colours %s with %s', (method, colour) => {
    const el = face('httpRequest', { method })
    expect(subtitle(el, 'HTTP Request')).toBe('API Call')
    expect(leaf(el, method, 'span').classList.contains(colour)).toBe(true)
  })

  it('HTTP Request is a GET by default, cuts the URL at 20 characters and shows {{output}}', () => {
    const el = face('httpRequest', {
      url: 'https://example.com/hooks/entry',
      outputVariable: 'resp',
    })
    expect(leaf(el, 'GET', 'span').classList.contains('text-green-500')).toBe(true)
    expect(has(el, 'https://example.com/...')).toBe(true)
    expect(has(el, '{{resp}}')).toBe(true)
    expect(has(face('httpRequest', { url: 'https://a.in/x' }), 'https://a.in/x')).toBe(true)
  })

  it('Telegram cuts the message at 25 characters, No message when empty', () => {
    const message = 'Entry placed for {{symbol}} at {{ltp}}'
    const el = face('telegramAlert', { message })
    expect(has(el, 'Telegram')).toBe(true)
    expect(has(el, `${message.substring(0, 25)}...`)).toBe(true)
    expect(has(face('telegramAlert', { message: 'y'.repeat(25) }), 'y'.repeat(25))).toBe(true)
    expect(has(face('telegramAlert'), 'No message')).toBe(true)
  })

  it('WhatsApp names the recipient, Self by default, and cuts the message at 25', () => {
    const el = face('whatsappAlert', { to: '+919876543210', message: 'Short note' })
    expect(subtitle(el, 'WhatsApp')).toBe('+919876543210')
    expect(has(el, 'Short note')).toBe(true)
    const message = 'Exit placed for {{symbol}} at {{ltp}}'
    const blank = face('whatsappAlert', { message })
    expect(subtitle(blank, 'WhatsApp')).toBe('Self')
    expect(has(blank, `${message.substring(0, 25)}...`)).toBe(true)
    expect(has(face('whatsappAlert'), 'No message')).toBe(true)
  })
})

describe('F-685 the group container', () => {
  /** The group's own box, inside xyflow's node wrapper. */
  const box = (el: HTMLElement) => el.firstElementChild as HTMLElement

  it('is a dashed box at least 200 by 150 with a target and a source handle', () => {
    const el = face('group')
    expect(box(el).className).toContain(
      'min-h-[150px] min-w-[200px] rounded-lg border-2 border-dashed'
    )
    expect(handles(el)).toEqual([
      { id: null, kind: 'target', position: 'top' },
      { id: null, kind: 'source', position: 'bottom' },
    ])
  })

  it.each([
    ['blue', 'border-blue-500/30 bg-blue-500/5'],
    ['green', 'border-buy/30 bg-buy/5'],
    ['red', 'border-sell/30 bg-sell/5'],
    ['purple', 'border-purple-500/30 bg-purple-500/5'],
    ['orange', 'border-orange-500/30 bg-orange-500/5'],
    ['default', 'border-border/50 bg-muted/20'],
    [undefined, 'border-border/50 bg-muted/20'],
    ['teal', 'border-border/50 bg-muted/20'],
  ])('a %s group is drawn with %s', (color, classes) => {
    expect(box(face('group', color === undefined ? {} : { color })).className).toContain(classes)
  })

  it('floats its label, Group by default, beside a layers icon', () => {
    const named = face('group', { label: 'Entry legs' })
    const title = leaf(named, 'Entry legs', 'span')
    expect(title.previousElementSibling?.classList.contains('lucide-layers')).toBe(true)
    expect(has(face('group'), 'Group')).toBe(true)
  })

  it('offers resize handles only while selected, never below 200 by 150', () => {
    const idle = face('group')
    expect(idle.querySelectorAll('.react-flow__resize-control')).toHaveLength(0)
    expect(box(idle).className).not.toContain('ring-2')

    const chosen = face('group', {}, true)
    expect(chosen.querySelectorAll('.react-flow__resize-control.handle')).toHaveLength(4)
    expect(chosen.querySelectorAll('.react-flow__resize-control.line')).toHaveLength(4)
    expect(box(chosen).className).toContain('ring-2 ring-primary')

    expect(resizer.props.at(-1)).toMatchObject({ minWidth: 200, minHeight: 150, isVisible: true })
    expect(resizer.props.some((p) => p.isVisible === false)).toBe(true)
  })
})
