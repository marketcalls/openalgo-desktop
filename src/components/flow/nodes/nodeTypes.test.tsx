/**
 * The node palette, the canvas registry and the defaults a dropped node
 * starts from are three lists in two files. A type missing from one of them
 * is a node a trader can drop but not see, see but not configure, or import
 * but not render. This pins the three to each other and to the web's 61
 * types, and renders every type on a real canvas from its defaults.
 */

import { render } from '@testing-library/react'
import { type Node, ReactFlow, ReactFlowProvider } from '@xyflow/react'
import { describe, expect, it } from 'vitest'
import { DEFAULT_NODE_DATA, NODE_DEFINITIONS } from '@/lib/flow/constants'
import { nodeTypes } from './index'

const registered = Object.keys(nodeTypes).sort()
const defaults = Object.keys(DEFAULT_NODE_DATA).sort()
const palette = Object.values(NODE_DEFINITIONS)
  .flat()
  .map((definition) => definition.type)

describe('the three node lists agree', () => {
  it('registers the web 61 node types', () => {
    expect(registered).toHaveLength(61)
  })

  it('gives every registered type a default and every default a component', () => {
    expect(defaults).toEqual(registered)
  })

  it('offers every registered type in the palette exactly once', () => {
    expect([...palette].sort()).toEqual(registered)
  })

  it('files the palette under the web categories', () => {
    expect(
      Object.fromEntries(Object.entries(NODE_DEFINITIONS).map(([k, v]) => [k, v.length]))
    ).toEqual({ TRIGGERS: 4, ACTIONS: 10, CONDITIONS: 9, DATA: 31, UTILITIES: 7 })
  })
})

describe('every node type renders from its defaults', () => {
  const nodes: Node[] = registered.map((type, index) => ({
    id: `n-${type}`,
    type,
    position: { x: (index % 8) * 220, y: Math.floor(index / 8) * 160 },
    data: { ...(DEFAULT_NODE_DATA as Record<string, Record<string, unknown>>)[type] },
  }))

  it('draws each one on the canvas with a title', () => {
    const { container } = render(
      <div style={{ width: 1600, height: 1200 }}>
        <ReactFlowProvider>
          <ReactFlow nodes={nodes} edges={[]} nodeTypes={nodeTypes} />
        </ReactFlowProvider>
      </div>
    )

    const drawn = [...container.querySelectorAll<HTMLElement>('.react-flow__node')]
    expect(drawn.map((el) => el.dataset.id).sort()).toEqual(nodes.map((n) => n.id).sort())
    for (const el of drawn) {
      expect(el.textContent?.trim(), el.dataset.id).not.toBe('')
    }
  })

  it('gives every node something to wire to', () => {
    const { container } = render(
      <div style={{ width: 1600, height: 1200 }}>
        <ReactFlowProvider>
          <ReactFlow nodes={nodes} edges={[]} nodeTypes={nodeTypes} />
        </ReactFlowProvider>
      </div>
    )
    for (const el of container.querySelectorAll<HTMLElement>('.react-flow__node')) {
      // Groups are containers; every other node has at least one handle.
      if (el.dataset.id === 'n-group') continue
      expect(el.querySelectorAll('.react-flow__handle').length, el.dataset.id).toBeGreaterThan(0)
    }
  })
})
