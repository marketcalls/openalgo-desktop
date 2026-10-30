/**
 * What the Flow editor hands its canvas, pinned with ReactFlow captured as in
 * FlowEditorViewport.test.tsx: the provider, the React Flow stylesheet, the
 * node and edge types, every canvas prop and child the web sets, and the
 * click handlers that select a node, an edge, or nothing.
 *
 * Pins: F-140 F-159 F-160 F-161.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, render, screen, waitFor } from '@testing-library/react'
import type { Edge, Node, ReactFlowProps } from '@xyflow/react'
import type { PropsWithChildren, ReactNode } from 'react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { edgeTypes } from '@/components/flow/edges'
import { nodeTypes } from '@/components/flow/nodes'
import { DEFAULT_NODE_DATA } from '@/lib/flow/constants'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

const captured = vi.hoisted(() => ({
  canvas: undefined as ReactFlowProps | undefined,
  background: undefined as Record<string, unknown> | undefined,
  miniMap: undefined as Record<string, unknown> | undefined,
  controls: 0,
  panel: undefined as Record<string, unknown> | undefined,
  stylesheet: false,
}))

/** The canvas's screen-to-flow conversion, identity unless a test says otherwise. */
const toFlow = vi.hoisted(() => vi.fn((position: { x: number; y: number }) => position))

vi.mock('@xyflow/react/dist/style.css', () => {
  captured.stylesheet = true
  return {}
})

vi.mock('@xyflow/react', async () => {
  const actual = await vi.importActual<typeof import('@xyflow/react')>('@xyflow/react')
  return {
    ...actual,
    ReactFlow: ({ children, ...props }: ReactFlowProps) => {
      captured.canvas = props
      return <div data-testid="flow-canvas">{children}</div>
    },
    ReactFlowProvider: ({ children }: PropsWithChildren) => (
      <div data-testid="flow-provider">{children}</div>
    ),
    Background: (props: Record<string, unknown>) => {
      captured.background = props
      return null
    },
    Controls: () => {
      captured.controls += 1
      return null
    },
    MiniMap: (props: Record<string, unknown>) => {
      captured.miniMap = props
      return null
    },
    Panel: ({ children, ...props }: { children: ReactNode }) => {
      captured.panel = props
      return <>{children}</>
    },
    useReactFlow: () => ({ screenToFlowPosition: toFlow }),
  }
})

const getWorkflow = vi.hoisted(() => vi.fn())
vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return { ...actual, getWorkflow, getIndexSymbolsLotSizes: vi.fn(async () => []) }
})

vi.mock('@/hooks/useProfileMenuItems', () => ({ useProfileMenuItems: () => [] }))

const { default: FlowEditor } = await import('./FlowEditor')

function renderEditor() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } },
  })
  return render(
    <MemoryRouter initialEntries={['/flow/editor/7']}>
      <QueryClientProvider client={client}>
        <Routes>
          <Route path="/flow/editor/:id" element={<FlowEditor />} />
        </Routes>
      </QueryClientProvider>
    </MemoryRouter>
  )
}

async function canvas() {
  await screen.findByDisplayValue('Opening range breakout')
  await waitFor(() => expect(captured.canvas?.nodes).toHaveLength(2))
  return captured.canvas as ReactFlowProps
}

const store = () => useFlowWorkflowStore.getState()
const click = {} as React.MouseEvent

beforeEach(() => {
  captured.canvas = undefined
  captured.background = undefined
  captured.miniMap = undefined
  captured.controls = 0
  captured.panel = undefined
  useFlowWorkflowStore.getState().resetWorkflow()
  toFlow.mockReset()
  toFlow.mockImplementation((position) => position)
  vi.restoreAllMocks()
  getWorkflow.mockReset()
  getWorkflow.mockResolvedValue({
    id: 7,
    name: 'Opening range breakout',
    description: null,
    nodes: [
      { id: 'node_1', type: 'start', position: { x: 0, y: 0 }, data: { time: '09:20' } },
      {
        id: 'node_2',
        type: 'placeOrder',
        position: { x: 0, y: 160 },
        data: { symbol: 'SBIN', exchange: 'NSE', action: 'BUY', quantity: 1 },
      },
    ],
    edges: [{ id: 'e1', source: 'node_1', target: 'node_2' }],
    is_active: false,
    schedule_job_id: null,
    webhook_token: null,
    webhook_secret: null,
    webhook_enabled: false,
    webhook_auth_type: 'payload',
    created_at: '2026-10-29T09:00:00',
    updated_at: '2026-10-29T09:00:00',
  })
})

describe('F-140 the editor setup', () => {
  it('loads the React Flow stylesheet', () => {
    expect(captured.stylesheet).toBe(true)
  })

  it('draws the canvas inside a ReactFlowProvider', async () => {
    renderEditor()
    await canvas()
    expect(screen.getByTestId('flow-provider')).toContainElement(screen.getByTestId('flow-canvas'))
  })

  it('registers the Flow node types and the insertable edge', async () => {
    renderEditor()
    const props = await canvas()
    expect(props.nodeTypes).toBe(nodeTypes)
    expect(props.edgeTypes).toBe(edgeTypes)
    expect(Object.keys(props.edgeTypes ?? {})).toEqual(['insertable'])
    expect(props.nodeTypes).toHaveProperty('start')
    expect(props.nodeTypes).toHaveProperty('placeOrder')
  })
})

describe('F-161 the canvas props', () => {
  it('sets exactly the viewport, grid, deletion, edge and connection props the web sets', async () => {
    renderEditor()
    const props = await canvas()
    expect(props.fitView).toBe(true)
    expect(props.fitViewOptions).toEqual({ maxZoom: 1 })
    expect(props.snapToGrid).toBe(true)
    expect(props.snapGrid).toEqual([16, 16])
    expect('deleteKeyCode' in props).toBe(true)
    expect(props.deleteKeyCode).toBeNull()
    expect(props.defaultEdgeOptions).toEqual({ type: 'insertable', animated: true })
    expect(props.connectionLineStyle).toEqual({ stroke: 'var(--color-primary)', strokeWidth: 2 })
    expect(props.connectionRadius).toBe(30)
    expect(props.connectOnClick).toBe(true)
  })

  it('hands the canvas the store graph and the store change handlers', async () => {
    renderEditor()
    const props = await canvas()
    expect(props.nodes).toBe(store().nodes)
    expect(props.edges).toBe(store().edges)
    expect(props.onNodesChange).toBe(store().onNodesChange)
    expect(props.onEdgesChange).toBe(store().onEdgesChange)
    expect(props.onConnect).toBe(store().onConnect)
  })

  it('draws the dotted background, the controls, the minimap and the status panel', async () => {
    renderEditor()
    await canvas()
    expect(captured.background).toEqual({ gap: 16, size: 1 })
    expect(captured.controls).toBeGreaterThan(0)
    expect(captured.miniMap).toEqual({ nodeStrokeWidth: 3, pannable: true, zoomable: true })
    expect(captured.panel).toMatchObject({ position: 'bottom-center', className: 'mb-4' })
    expect(screen.getByText('Workflow inactive')).toBeInTheDocument()
  })
})

describe('F-160 clicking on the canvas', () => {
  it('a node click selects the node and opens its settings', async () => {
    renderEditor()
    const props = await canvas()
    act(() => props.onNodeClick?.(click, store().nodes[1] as Node))
    expect(store().selectedNodeId).toBe('node_2')
    expect(await screen.findByRole('heading', { level: 2, name: 'Place Order' })).toBeVisible()
  })

  it('an edge click selects the edge and clears the node', async () => {
    renderEditor()
    const props = await canvas()
    act(() => store().selectNode('node_2'))
    act(() => props.onEdgeClick?.(click, store().edges[0] as Edge))
    expect(store().selectedEdgeId).toBe('e1')
    expect(store().selectedNodeId).toBeNull()
  })

  it('a click on the empty canvas clears the selected node and the selected edge', async () => {
    renderEditor()
    const props = await canvas()
    act(() => store().selectNode('node_2'))
    act(() => props.onPaneClick?.(click))
    expect(store().selectedNodeId).toBeNull()
    await waitFor(() =>
      expect(
        screen.queryByRole('heading', { level: 2, name: 'Place Order' })
      ).not.toBeInTheDocument()
    )

    act(() => store().selectEdge('e1'))
    act(() => props.onPaneClick?.(click))
    expect(store().selectedEdgeId).toBeNull()
  })
})

describe('F-159 adding a node from the palette without a pointer', () => {
  /** The box the canvas wrapper reports, or none at all. */
  function canvasBox(box: Partial<DOMRect> | undefined) {
    const wrapper = screen.getByTestId('flow-canvas').parentElement as HTMLElement
    vi.spyOn(wrapper, 'getBoundingClientRect').mockReturnValue(box as DOMRect)
  }

  it('places it at the middle of the visible canvas, in flow coordinates, and selects it', async () => {
    renderEditor()
    await canvas()
    canvasBox({ x: 224, y: 56, width: 800, height: 600 })
    // A canvas panned and zoomed: flow coordinates differ from the screen's.
    toFlow.mockImplementation(({ x, y }) => ({ x: (x - 100) / 2, y: (y - 40) / 2 }))
    act(() => screen.getByRole('button', { name: 'Add Price Alert node' }).click())

    expect(toFlow).toHaveBeenLastCalledWith({ x: 624, y: 356 })
    const added = store().nodes.at(-1)
    expect(added).toMatchObject({
      id: 'node_3',
      type: 'priceAlert',
      position: { x: 262, y: 158 },
      data: DEFAULT_NODE_DATA.priceAlert,
    })
    expect(added?.data).not.toBe(DEFAULT_NODE_DATA.priceAlert)
    expect(store().selectedNodeId).toBe('node_3')
    expect(store().isModified).toBe(true)
  })

  it('falls back to the middle of the window when the canvas has no box', async () => {
    renderEditor()
    await canvas()
    canvasBox(undefined)
    act(() => screen.getByRole('button', { name: 'Add Order Update node' }).click())

    expect(toFlow).toHaveBeenLastCalledWith({
      x: window.innerWidth / 2,
      y: window.innerHeight / 2,
    })
    expect(store().nodes.at(-1)).toMatchObject({
      type: 'orderUpdateTrigger',
      position: { x: window.innerWidth / 2, y: window.innerHeight / 2 },
    })
    expect(store().selectedNodeId).toBe(store().nodes.at(-1)?.id)
  })
})
