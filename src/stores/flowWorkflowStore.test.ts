/**
 * The Flow editor's graph store, beyond the save race the web's own test
 * covers (flowWorkflowStore.saveRace.test.ts). What a trader edits on the
 * canvas is what Save, Run Now and Activate send, so every edit must mark
 * the workflow unsaved and a node's removal must take its wires with it.
 */

import type { Edge, Node } from '@xyflow/react'
import { beforeEach, describe, expect, it } from 'vitest'
import { useFlowWorkflowStore } from './flowWorkflowStore'

const store = () => useFlowWorkflowStore.getState()

function node(id: string, type = 'placeOrder'): Node {
  return { id, type, position: { x: 0, y: 0 }, data: { symbol: 'SBIN' } }
}

function edge(id: string, source: string, target: string): Edge {
  return { id, source, target }
}

/** A start node wired to two orders, as loaded from the server. */
function loadThreeNodeGraph() {
  store().setWorkflow({
    id: 7,
    name: 'Opening range',
    description: 'NIFTY breakout',
    nodes: [node('start', 'start'), node('buy'), node('sell')],
    edges: [edge('e1', 'start', 'buy'), edge('e2', 'start', 'sell'), edge('e3', 'buy', 'sell')],
  })
}

beforeEach(() => store().resetWorkflow())

describe('loading and resetting a workflow', () => {
  it('starts as an untitled, empty, saved workflow', () => {
    expect(store()).toMatchObject({
      id: null,
      name: 'Untitled Workflow',
      description: '',
      nodes: [],
      edges: [],
      selectedNodeId: null,
      selectedEdgeId: null,
      isModified: false,
    })
  })

  it('loads a stored workflow as saved, with nothing selected', () => {
    store().selectNode('stale')
    loadThreeNodeGraph()
    expect(store()).toMatchObject({
      id: 7,
      name: 'Opening range',
      description: 'NIFTY breakout',
      isModified: false,
      selectedNodeId: null,
      selectedEdgeId: null,
    })
    expect(store().nodes.map((n) => n.id)).toEqual(['start', 'buy', 'sell'])
    expect(store().edges).toHaveLength(3)
  })

  it('reset forgets the previous workflow entirely', () => {
    loadThreeNodeGraph()
    store().setName('Edited')
    store().resetWorkflow()
    expect(store()).toMatchObject({ id: null, name: 'Untitled Workflow', nodes: [], edges: [] })
    expect(store().isModified).toBe(false)
  })
})

describe('every edit marks the workflow unsaved', () => {
  beforeEach(loadThreeNodeGraph)

  it.each([
    ['renaming', () => store().setName('Renamed')],
    ['describing', () => store().setDescription('New notes')],
    ['replacing the nodes', () => store().setNodes([node('only')])],
    ['replacing the edges', () => store().setEdges([])],
    ['adding a node', () => store().addNode(node('extra'))],
    ['editing a node', () => store().updateNodeData('buy', { quantity: 5 })],
    ['deleting a node', () => store().deleteNode('sell')],
    ['deleting a wire', () => store().deleteEdge('e1')],
    ['wiring two nodes', () => store().onConnect({ source: 'sell', target: 'buy' } as never)],
  ])('%s', (_label, edit) => {
    expect(store().isModified).toBe(false)
    edit()
    expect(store().isModified).toBe(true)
  })

  it('a rename and a description keep their text', () => {
    store().setName('Renamed')
    store().setDescription('New notes')
    expect(store().name).toBe('Renamed')
    expect(store().description).toBe('New notes')
  })
})

describe('canvas changes that are not edits', () => {
  beforeEach(loadThreeNodeGraph)

  it('measuring a node or selecting it leaves the workflow saved', () => {
    store().onNodesChange([
      { type: 'dimensions', id: 'buy', dimensions: { width: 120, height: 60 } },
      { type: 'select', id: 'buy', selected: true },
    ])
    store().onEdgesChange([{ type: 'select', id: 'e1', selected: true }])
    expect(store().isModified).toBe(false)
    expect(store().nodes.find((n) => n.id === 'buy')?.selected).toBe(true)
  })

  it('moving a node is an edit', () => {
    store().onNodesChange([{ type: 'position', id: 'buy', position: { x: 40, y: 80 } }])
    expect(store().isModified).toBe(true)
    expect(store().nodes.find((n) => n.id === 'buy')?.position).toEqual({ x: 40, y: 80 })
  })

  it('removing a wire through the canvas is an edit', () => {
    store().onEdgesChange([{ type: 'remove', id: 'e2' }])
    expect(store().isModified).toBe(true)
    expect(store().edges.map((e) => e.id)).toEqual(['e1', 'e3'])
  })

  it('a later selection does not clear an earlier edit', () => {
    store().onNodesChange([{ type: 'position', id: 'buy', position: { x: 1, y: 1 } }])
    store().onNodesChange([{ type: 'select', id: 'buy', selected: true }])
    expect(store().isModified).toBe(true)
  })
})

describe('nodes', () => {
  beforeEach(loadThreeNodeGraph)

  it('editing a node merges into its data and leaves the others alone', () => {
    store().updateNodeData('buy', { quantity: 5, symbol: 'INFY' })
    expect(store().nodes.find((n) => n.id === 'buy')?.data).toEqual({
      symbol: 'INFY',
      quantity: 5,
    })
    expect(store().nodes.find((n) => n.id === 'sell')?.data).toEqual({ symbol: 'SBIN' })
  })

  it('deleting a node removes every wire into or out of it', () => {
    store().deleteNode('buy')
    expect(store().nodes.map((n) => n.id)).toEqual(['start', 'sell'])
    expect(store().edges.map((e) => e.id)).toEqual(['e2'])
  })

  it('deleting the selected node clears the selection; another keeps it', () => {
    store().selectNode('sell')
    store().deleteNode('buy')
    expect(store().selectedNodeId).toBe('sell')
    store().deleteNode('sell')
    expect(store().selectedNodeId).toBeNull()
  })
})

describe('wires', () => {
  beforeEach(loadThreeNodeGraph)

  it('a new wire is an animated insertable edge with its own id', () => {
    store().onConnect({
      source: 'sell',
      target: 'buy',
      sourceHandle: null,
      targetHandle: null,
    })
    const added = store().edges.find((e) => e.source === 'sell' && e.target === 'buy')
    expect(added).toMatchObject({ type: 'insertable', animated: true })
    expect(added?.id).toMatch(/^edge-\d+$/)
  })

  it('deleting the selected wire clears the selection', () => {
    store().selectEdge('e3')
    store().deleteEdge('e3')
    expect(store().selectedEdgeId).toBeNull()
    expect(store().edges.map((e) => e.id)).toEqual(['e1', 'e2'])
  })

  it('deleting another wire keeps the selection', () => {
    store().selectEdge('e3')
    store().deleteEdge('e1')
    expect(store().selectedEdgeId).toBe('e3')
  })
})

describe('selection', () => {
  beforeEach(loadThreeNodeGraph)

  it('a node and a wire are never selected together', () => {
    store().selectEdge('e1')
    store().selectNode('buy')
    expect(store()).toMatchObject({ selectedNodeId: 'buy', selectedEdgeId: null })
    store().selectEdge('e2')
    expect(store()).toMatchObject({ selectedNodeId: null, selectedEdgeId: 'e2' })
  })

  it('Delete on a selected node removes it and its wires', () => {
    store().selectNode('start')
    store().deleteSelected()
    expect(store().nodes.map((n) => n.id)).toEqual(['buy', 'sell'])
    expect(store().edges.map((e) => e.id)).toEqual(['e3'])
    expect(store().selectedNodeId).toBeNull()
    expect(store().isModified).toBe(true)
  })

  it('Delete on a selected wire removes only that wire', () => {
    store().selectEdge('e1')
    store().deleteSelected()
    expect(store().nodes).toHaveLength(3)
    expect(store().edges.map((e) => e.id)).toEqual(['e2', 'e3'])
    expect(store().selectedEdgeId).toBeNull()
    expect(store().isModified).toBe(true)
  })

  it('Delete with nothing selected changes nothing', () => {
    const before = store()
    store().deleteSelected()
    expect(store().nodes).toBe(before.nodes)
    expect(store().edges).toBe(before.edges)
    expect(store().isModified).toBe(false)
  })
})
