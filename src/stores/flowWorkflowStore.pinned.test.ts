/**
 * Two store rules the audit found unpinned (parity F-191, F-195).
 */

import { beforeEach, describe, expect, it } from 'vitest'
import { useFlowWorkflowStore } from './flowWorkflowStore'

const store = () => useFlowWorkflowStore.getState()

beforeEach(() => store().resetWorkflow())

describe('loading a workflow over unsaved edits (F-191)', () => {
  it('replaces the canvas and clears the unsaved mark and both selections', () => {
    store().setName('Edited, not saved')
    store().selectEdge('stale-edge')
    expect(store().isModified).toBe(true)
    store().setWorkflow({ id: 9, name: 'Stored', description: '', nodes: [], edges: [] })
    expect(store()).toMatchObject({
      id: 9,
      name: 'Stored',
      isModified: false,
      selectedNodeId: null,
      selectedEdgeId: null,
    })
  })
})

describe('wiring the same two handles twice (F-195)', () => {
  it('keeps one wire per source, target and handle pair', () => {
    store().setWorkflow({
      id: 1,
      name: 'W',
      description: '',
      nodes: [
        { id: 'a', type: 'positionCheck', position: { x: 0, y: 0 }, data: {} },
        { id: 'b', type: 'placeOrder', position: { x: 0, y: 100 }, data: {} },
      ],
      edges: [],
    })
    const wire = { source: 'a', target: 'b', sourceHandle: 'true', targetHandle: null }
    store().onConnect(wire)
    store().onConnect(wire)
    expect(store().edges).toHaveLength(1)
    // A different branch from the same node is a different wire.
    store().onConnect({ ...wire, sourceHandle: 'false' })
    expect(
      store()
        .edges.map((e) => e.sourceHandle)
        .sort()
    ).toEqual(['false', 'true'])
  })
})
