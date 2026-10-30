/**
 * The insertable edge, pinned as the web draws and wires it (parity F-690,
 * F-691, F-692): the smooth-step path and its stroke, the dash animation, the
 * "+" button's place and when it shows, the exact insert menu, and what an
 * insert writes (the new node's id, position and data, the two wires and the
 * handles they keep). The desktop's pass-branch fix is pinned in
 * InsertableEdge.test.tsx.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { type Edge, type Node, ReactFlow, ReactFlowProvider, useReactFlow } from '@xyflow/react'
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest'
import { nodeTypes } from '@/components/flow/nodes'
import { DEFAULT_NODE_DATA } from '@/lib/flow/constants'
import { edgeTypes } from './index'

/** What getSmoothStepPath returned, so the drawn path can be checked against it. */
const smoothStep = vi.hoisted(() => ({
  calls: [] as Array<{ params: unknown; result: [string, number, number] }>,
}))

vi.mock('@xyflow/react', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@xyflow/react')>()
  return {
    ...actual,
    getSmoothStepPath: (params: Parameters<typeof actual.getSmoothStepPath>[0]) => {
      const result = actual.getSmoothStepPath(params)
      smoothStep.calls.push({ params, result: [result[0], result[1], result[2]] })
      return result
    },
  }
})

/**
 * xyflow draws an edge only between measured nodes, and jsdom measures
 * nothing: the stand-ins from xyflow's testing guide, as in
 * InsertableEdge.test.tsx.
 */
class MeasuringResizeObserver {
  private readonly callback: ResizeObserverCallback
  constructor(callback: ResizeObserverCallback) {
    this.callback = callback
  }
  observe(target: Element) {
    this.callback([{ target } as ResizeObserverEntry], this as unknown as ResizeObserver)
  }
  unobserve() {}
  disconnect() {}
}

class MatrixStub {
  m22: number
  constructor(transform?: string) {
    const scale = transform?.match(/scale\(([1-9.])\)/)?.[1]
    this.m22 = scale !== undefined ? Number(scale) : 1
  }
}

beforeAll(() => {
  vi.stubGlobal('ResizeObserver', MeasuringResizeObserver)
  vi.stubGlobal('DOMMatrixReadOnly', MatrixStub)
  Object.defineProperties(HTMLElement.prototype, {
    offsetHeight: {
      configurable: true,
      get() {
        return Number.parseFloat((this as HTMLElement).style.height) || 1
      },
    },
    offsetWidth: {
      configurable: true,
      get() {
        return Number.parseFloat((this as HTMLElement).style.width) || 1
      },
    },
  })
})

afterAll(() => vi.unstubAllGlobals())

beforeEach(() => {
  smoothStep.calls = []
})

const NOW = 1767225600000

/** A wire out of a branch into a numbered gate input, so both kept handles show. */
const NODES: Node[] = [
  { id: 'check', type: 'positionCheck', position: { x: 100, y: 0 }, data: { symbol: 'SBIN' } },
  { id: 'gate', type: 'andGate', position: { x: 300, y: 300 }, data: { inputCount: 2 } },
]
const WIRE: Edge = {
  id: 'e1',
  source: 'check',
  sourceHandle: 'true',
  target: 'gate',
  targetHandle: 'input-1',
  type: 'insertable',
}

let flow: ReturnType<typeof useReactFlow> | null = null
function Capture() {
  flow = useReactFlow()
  return null
}

function renderCanvas(edge: Partial<Edge> = {}) {
  return render(
    <div style={{ width: 800, height: 600 }}>
      <ReactFlowProvider>
        <ReactFlow
          defaultNodes={NODES}
          defaultEdges={[{ ...WIRE, ...edge }]}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
        />
        <Capture />
      </ReactFlowProvider>
    </div>
  )
}

const plus = () => screen.findByRole('button', { name: 'Insert a node on this connection' })

function tokens(el: Element): string[] {
  return el.getAttribute('class')?.split(/\s+/) ?? []
}

async function drawnPath(container: HTMLElement): Promise<SVGPathElement> {
  await plus()
  const path = container.querySelector<SVGPathElement>('path.react-flow__edge-path')
  if (!path) throw new Error('edge not drawn')
  return path
}

/** Open the + menu, open a category, and click one of its items. */
async function insert(category: string, label: string) {
  const user = userEvent.setup()
  await user.click(await plus())
  await user.click(await screen.findByRole('menuitem', { name: category }))
  const item = await screen.findByRole('menuitem', { name: label })
  // The insert reads the clock synchronously inside the click handler.
  const clock = vi.spyOn(Date, 'now').mockReturnValue(NOW)
  // A click without the pointer travelling there: user-event's pointer path
  // from the sub-trigger leaves Radix's grace area and closes the submenu.
  fireEvent.click(item)
  clock.mockRestore()
}

describe('F-692 inserting a node on a wire', () => {
  // First in the file on purpose: the id counter is module state that starts at 1000.
  it('numbers each new node node_{now}_{counter}, the counter starting at 1000', async () => {
    const first = renderCanvas()
    await insert('Actions', 'Place Order')
    await waitFor(() => expect(flow?.getNodes()).toHaveLength(3))
    expect(flow?.getNodes().map((n) => n.id)).toEqual(['check', 'gate', `node_${NOW}_1000`])
    first.unmount()

    renderCanvas()
    await insert('Data', 'Get Quote')
    await waitFor(() => expect(flow?.getNodes()).toHaveLength(3))
    expect(flow?.getNodes()[2].id).toBe(`node_${NOW}_1001`)
  }, 20000)

  it('places it 50px below the midpoint of the two nodes, with a copy of its defaults', async () => {
    renderCanvas()
    await insert('Actions', 'Place Order')
    await waitFor(() => expect(flow?.getNodes()).toHaveLength(3))
    const inserted = flow?.getNodes()[2]
    expect(inserted?.type).toBe('placeOrder')
    expect(inserted?.position).toEqual({ x: 200, y: 200 })
    expect(inserted?.data).toEqual(DEFAULT_NODE_DATA.placeOrder)
    expect(inserted?.data).not.toBe(DEFAULT_NODE_DATA.placeOrder)
  })

  it('replaces the wire with two animated insertable wires that keep its handles', async () => {
    renderCanvas()
    await insert('Utility', 'Log')
    await waitFor(() => expect(flow?.getNodes()).toHaveLength(3))
    const id = flow?.getNodes()[2].id
    const wires = flow?.getEdges().map((e) => ({
      id: e.id,
      source: e.source,
      sourceHandle: e.sourceHandle,
      target: e.target,
      targetHandle: e.targetHandle,
      type: e.type,
      animated: e.animated,
    }))
    expect(wires).toEqual([
      {
        id: `edge-${NOW}-1`,
        source: 'check',
        sourceHandle: 'true',
        target: id,
        targetHandle: undefined,
        type: 'insertable',
        animated: true,
      },
      {
        id: `edge-${NOW}-2`,
        source: id,
        sourceHandle: undefined,
        target: 'gate',
        targetHandle: 'input-1',
        type: 'insertable',
        animated: true,
      },
    ])
  })

  it('closes the menu once the node is in', async () => {
    renderCanvas()
    await insert('Utility', 'Variable')
    await waitFor(() => expect(flow?.getNodes()).toHaveLength(3))
    await waitFor(() => expect(screen.queryByRole('menu')).toBeNull())
  })
})

describe('F-690 how the wire is drawn', () => {
  it('draws the smooth-step path between the two handles', async () => {
    const { container } = renderCanvas()
    const path = await drawnPath(container)
    const last = smoothStep.calls.at(-1)
    expect(last?.params).toMatchObject({ sourcePosition: 'bottom', targetPosition: 'top' })
    expect(path.getAttribute('d')).toBe(last?.result[0])
    expect(path.getAttribute('fill')).toBe('none')
  })

  it('strokes a plain wire 2px #888888, solid and still', async () => {
    const { container } = renderCanvas()
    const path = await drawnPath(container)
    expect(path.getAttribute('stroke-width')).toBe('2')
    expect(path.getAttribute('stroke')).toBe('#888888')
    expect(path.getAttribute('stroke-dasharray')).toBeNull()
    expect(path.style.animation).toBe('')
  })

  it('strokes a selected wire 3px #3b82f6', async () => {
    const { container } = renderCanvas({ selected: true })
    const path = await drawnPath(container)
    expect(path.getAttribute('stroke-width')).toBe('3')
    expect(path.getAttribute('stroke')).toBe('#3b82f6')
  })

  it('strokes a hovered wire 3px #3b82f6 and drops back on leaving', async () => {
    const { container } = renderCanvas()
    const path = await drawnPath(container)
    const hotspot = (await plus()).parentElement as HTMLElement
    fireEvent.mouseEnter(hotspot)
    await waitFor(() => expect(path.getAttribute('stroke')).toBe('#3b82f6'))
    expect(path.getAttribute('stroke-width')).toBe('3')
    fireEvent.mouseLeave(hotspot)
    await waitFor(() => expect(path.getAttribute('stroke')).toBe('#888888'))
    expect(path.getAttribute('stroke-width')).toBe('2')
  })

  it('dashes an animated wire 5 5 and runs dashdraw 0.5s linear infinite', async () => {
    const { container } = renderCanvas({ animated: true })
    const path = await drawnPath(container)
    expect(path.getAttribute('stroke-dasharray')).toBe('5 5')
    expect(path.style.animation).toBe('dashdraw 0.5s linear infinite')
  })

  it('has the dashdraw keyframes in the app stylesheet', () => {
    const css = readFileSync(join(process.cwd(), 'src/index.css'), 'utf8')
    expect(css).toMatch(/@keyframes dashdraw\s*\{\s*from\s*\{\s*stroke-dashoffset:\s*10;/)
  })

  it('centres a 20px round + at the label point, outside drag and pan', async () => {
    renderCanvas()
    const button = await plus()
    const hotspot = button.parentElement as HTMLElement
    const [, labelX, labelY] = smoothStep.calls.at(-1)?.result ?? []
    expect(hotspot.style.transform).toBe(
      `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)`
    )
    expect(tokens(hotspot)).toEqual(
      expect.arrayContaining(['nodrag', 'nopan', 'pointer-events-auto', 'absolute'])
    )
    expect(tokens(button)).toEqual(expect.arrayContaining(['h-5', 'w-5', 'rounded-full']))
    expect(button.querySelector('svg')?.classList.contains('lucide-plus')).toBe(true)
  })

  it('hides the + until the wire is hovered, and hides it again on leaving', async () => {
    renderCanvas()
    const button = await plus()
    expect(tokens(button)).toEqual(expect.arrayContaining(['opacity-0', 'scale-75']))
    fireEvent.mouseEnter(button.parentElement as HTMLElement)
    await waitFor(() =>
      expect(tokens(button)).toEqual(expect.arrayContaining(['opacity-100', 'scale-100']))
    )
    expect(tokens(button)).not.toContain('opacity-0')
    fireEvent.mouseLeave(button.parentElement as HTMLElement)
    await waitFor(() => expect(tokens(button)).toContain('opacity-0'))
  })

  it('shows the + on a selected wire', async () => {
    renderCanvas({ selected: true })
    const button = await plus()
    expect(tokens(button)).toEqual(expect.arrayContaining(['opacity-100', 'scale-100']))
    expect(tokens(button)).not.toContain('opacity-0')
  })

  it('keeps the + shown while its menu is open, even after the pointer leaves', async () => {
    renderCanvas()
    const button = await plus()
    fireEvent.keyDown(button, { key: 'Enter' })
    await screen.findByRole('menu')
    expect(tokens(button)).toContain('opacity-100')
    fireEvent.mouseLeave(button.parentElement as HTMLElement)
    expect(tokens(button)).toContain('opacity-100')
  })
})

describe('F-691 the insert menu', () => {
  async function openMenu() {
    const user = userEvent.setup()
    await user.click(await plus())
    return user
  }

  it('offers Actions, Logic, Data and Utility, in that order', async () => {
    renderCanvas()
    await openMenu()
    const root = await screen.findByRole('menu')
    expect(
      within(root)
        .getAllByRole('menuitem')
        .map((item) => item.textContent)
    ).toEqual(['Actions', 'Logic', 'Data', 'Utility'])
  })

  it.each([
    [
      'Actions',
      [
        'Place Order',
        'Smart Order',
        'Options Order',
        'Multi-Leg Options',
        'Close Positions',
        'Cancel All',
      ],
    ],
    ['Logic', ['Time Condition', 'Position Check', 'Fund Check', 'Price Check', 'Time Window']],
    ['Data', ['Get Quote', 'Get Depth', 'Open Position']],
    ['Utility', ['Wait Until', 'Delay', 'Telegram Alert', 'Log', 'Variable']],
  ])('%s lists exactly %o', async (category, labels) => {
    renderCanvas()
    const user = await openMenu()
    await user.click(await screen.findByRole('menuitem', { name: category }))
    await waitFor(() => expect(screen.getAllByRole('menu')).toHaveLength(2))
    const submenu = screen.getAllByRole('menu')[1]
    expect(
      within(submenu)
        .getAllByRole('menuitem')
        .map((item) => item.textContent)
    ).toEqual(labels)
  })

  it.each([
    ['Actions', 'Smart Order', 'smartOrder'],
    ['Actions', 'Options Order', 'optionsOrder'],
    ['Actions', 'Multi-Leg Options', 'optionsMultiOrder'],
    ['Actions', 'Close Positions', 'closePositions'],
    ['Actions', 'Cancel All', 'cancelAllOrders'],
    ['Data', 'Get Depth', 'getDepth'],
    ['Data', 'Open Position', 'openPosition'],
    ['Utility', 'Wait Until', 'waitUntil'],
    ['Utility', 'Telegram Alert', 'telegramAlert'],
  ])('%s > %s inserts a %s node', async (category, label, type) => {
    renderCanvas()
    await insert(category, label)
    await waitFor(() => expect(flow?.getNodes()).toHaveLength(3))
    const inserted = flow?.getNodes()[2]
    expect(inserted?.type).toBe(type)
    expect(inserted?.data).toEqual((DEFAULT_NODE_DATA as Record<string, unknown>)[type])
  })
})
