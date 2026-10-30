/**
 * Node faces that said something the run does not do (parity F-677, F-684,
 * F-586), on a real canvas.
 */

import { render } from '@testing-library/react'
import { type Node, ReactFlow, ReactFlowProvider } from '@xyflow/react'
import { describe, expect, it } from 'vitest'
import { nodeTypes } from './index'

function face(node: Node): HTMLElement {
  const { container } = render(
    <div style={{ width: 800, height: 600 }}>
      <ReactFlowProvider>
        <ReactFlow nodes={[node]} edges={[]} nodeTypes={nodeTypes} />
      </ReactFlowProvider>
    </div>
  )
  const el = container.querySelector<HTMLElement>(`.react-flow__node[data-id="${node.id}"]`)
  if (!el) throw new Error('node not drawn')
  return el
}

describe('node faces', () => {
  it('a split order advertises no delay the run never makes (F-677)', () => {
    const el = face({
      id: 's',
      type: 'splitOrder',
      position: { x: 0, y: 0 },
      data: { symbol: 'SBIN', quantity: 100, splitSize: 25, delayMs: 500 },
    })
    expect(el.textContent).not.toMatch(/delay/)
    expect(el.textContent).toContain('100 / 25')
  })

  it('a split order with no split size shows the run default of 10 (F-586)', () => {
    const el = face({
      id: 's2',
      type: 'splitOrder',
      position: { x: 0, y: 0 },
      data: { symbol: 'SBIN', quantity: 100 },
    })
    expect(el.textContent).toContain('100 / 10')
  })

  it('a Telegram alert does not read "No user" when delivery works (F-684)', () => {
    const el = face({
      id: 't',
      type: 'telegramAlert',
      position: { x: 0, y: 0 },
      data: { message: 'Order placed' },
    })
    expect(el.textContent).not.toContain('No user')
    expect(el.textContent).toContain('Linked account')
  })
})
