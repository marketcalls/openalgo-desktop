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

describe('starting over (F-191)', () => {
  it('restores the blank workflow: no description and nothing selected', () => {
    store().setWorkflow({ id: 9, name: 'Stored', description: 'NIFTY', nodes: [], edges: [] })
    store().selectNode('n1')
    store().selectEdge('e1')
    store().resetWorkflow()
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
})

describe("updating a node's data (F-194)", () => {
  it('merges one level deep: a nested object or list is replaced, not merged', () => {
    store().setWorkflow({
      id: 1,
      name: 'w',
      description: '',
      nodes: [
        {
          id: 'n1',
          type: 'optionsMultiOrder',
          position: { x: 0, y: 0 },
          data: {
            underlying: 'NIFTY',
            legs: [{ offset: 'ATM' }, { offset: 'OTM2' }],
            meta: { a: 1, b: 2 },
          },
        },
      ],
      edges: [],
    })
    store().updateNodeData('n1', { legs: [{ offset: 'ITM1' }], meta: { a: 3 } })
    expect(store().nodes[0].data).toEqual({
      underlying: 'NIFTY',
      legs: [{ offset: 'ITM1' }],
      meta: { a: 3 },
    })
    expect(store().isModified).toBe(true)
  })
})
