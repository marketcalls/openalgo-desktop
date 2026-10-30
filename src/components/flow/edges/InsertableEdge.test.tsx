/**
 * The "+" on a wire inserts a node between the two it joins. A Logic node
 * inserted there must continue from its pass branch (parity F-693): the web
 * wired its output with no branch, which the canvas cannot draw and the run
 * treats as pass-through, so the order after it ran whether or not the
 * condition held.
 */

import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { type Edge, type Node, ReactFlow, ReactFlowProvider, useReactFlow } from '@xyflow/react'
import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest'
import { nodeTypes } from '@/components/flow/nodes'
import { passBranchHandle } from '@/lib/desktopFlow'
import { checkA11y } from '@/test/a11y-utils'
import { edgeTypes } from './index'

/**
 * xyflow draws an edge only between measured nodes, and jsdom measures
 * nothing. These are the stand-ins xyflow's own testing guide gives for
 * jsdom: an observer that reports at once, element sizes read from the
 * inline style, and the matrix the viewport reads its zoom from. Scoped to
 * this file (vitest isolates test files).
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

const NODES: Node[] = [
  { id: 'start', type: 'start', position: { x: 0, y: 0 }, data: { scheduleType: 'daily' } },
  { id: 'buy', type: 'placeOrder', position: { x: 0, y: 300 }, data: { symbol: 'SBIN' } },
]
const EDGES: Edge[] = [{ id: 'e1', source: 'start', target: 'buy', type: 'insertable' }]

let flow: ReturnType<typeof useReactFlow> | null = null
function Capture() {
  flow = useReactFlow()
  return null
}

function renderCanvas() {
  return render(
    <div style={{ width: 800, height: 600 }}>
      <ReactFlowProvider>
        <ReactFlow
          defaultNodes={NODES}
          defaultEdges={EDGES}
          nodeTypes={nodeTypes}
          edgeTypes={edgeTypes}
        />
        <Capture />
      </ReactFlowProvider>
    </div>
  )
}

async function insert(category: string, label: string) {
  const user = userEvent.setup()
  await user.click(await screen.findByRole('button', { name: 'Insert a node on this connection' }))
  await user.click(await screen.findByRole('menuitem', { name: category }))
  // A click without the pointer travelling there: user-event's pointer path
  // from the sub-trigger leaves Radix's grace area and closes the submenu.
  fireEvent.click(await screen.findByRole('menuitem', { name: label }))
}

function wiresAround(inserted: string) {
  const edges = flow?.getEdges() ?? []
  return {
    into: edges.find((e) => e.target === inserted),
    out: edges.find((e) => e.source === inserted),
    all: edges,
  }
}

describe('inserting a node on a wire', () => {
  it.each([
    ['Time Condition', 'timeCondition', 'yes'],
    ['Position Check', 'positionCheck', 'true'],
    ['Fund Check', 'fundCheck', 'true'],
    ['Price Check', 'priceCondition', 'true'],
    ['Time Window', 'timeWindow', 'true'],
  ])('a %s continues from its pass branch', async (label, type, handle) => {
    renderCanvas()
    await insert('Logic', label)
    await waitFor(() => expect(flow?.getNodes()).toHaveLength(3))
    const inserted = flow?.getNodes().find((n) => n.type === type)
    expect(inserted).toBeDefined()
    const { into, out, all } = wiresAround(inserted?.id ?? '')
    expect(all.find((e) => e.id === 'e1')).toBeUndefined()
    expect(into).toMatchObject({ source: 'start' })
    expect(out).toMatchObject({ target: 'buy', sourceHandle: handle })
    expect(passBranchHandle(type)).toBe(handle)
  })

  it('a node with one output keeps a plain wire', async () => {
    renderCanvas()
    await insert('Utility', 'Delay')
    await waitFor(() => expect(flow?.getNodes()).toHaveLength(3))
    const inserted = flow?.getNodes().find((n) => n.type === 'delay')
    expect(wiresAround(inserted?.id ?? '').out?.sourceHandle).toBeUndefined()
  })

  it('names the insert button for a screen reader', async () => {
    const { container } = renderCanvas()
    const button = await screen.findByRole('button', { name: 'Insert a node on this connection' })
    expect(button.className).toContain('focus-visible:opacity-100')
    await checkA11y(button.parentElement ?? container)
  })
})

describe('passBranchHandle', () => {
  it('names a pass branch for every node with branch outputs only', () => {
    for (const type of ['andGate', 'orGate', 'varCondition', 'priceAlert']) {
      expect(passBranchHandle(type)).toBe('true')
    }
    expect(passBranchHandle('notGate')).toBe('yes')
    expect(passBranchHandle('placeOrder')).toBeUndefined()
  })
})
