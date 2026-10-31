/**
 * Dragging a node from the palette onto the Flow canvas, pinned with ReactFlow
 * captured as in FlowEditorCanvas.pinned.test.tsx: the drag offers a move,
 * the canvas accepts it as a move, the drop lands where the pointer is in flow
 * coordinates, and a type with no defaults starts with empty data.
 *
 * Pins: F-158.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import type { ReactFlowProps } from '@xyflow/react'
import type { PropsWithChildren, ReactNode } from 'react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { DEFAULT_NODE_DATA } from '@/lib/flow/constants'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

const captured = vi.hoisted(() => ({
  canvas: undefined as ReactFlowProps | undefined,
}))

/** The canvas's screen-to-flow conversion, identity unless a test says otherwise. */
const toFlow = vi.hoisted(() => vi.fn((position: { x: number; y: number }) => position))

vi.mock('@xyflow/react', async () => {
  const actual = await vi.importActual<typeof import('@xyflow/react')>('@xyflow/react')
  return {
    ...actual,
    ReactFlow: ({ children, ...props }: ReactFlowProps) => {
      captured.canvas = props
      return <div data-testid="flow-canvas">{children}</div>
    },
    ReactFlowProvider: ({ children }: PropsWithChildren) => <>{children}</>,
    Background: () => null,
    Controls: () => null,
    MiniMap: () => null,
    Panel: ({ children }: { children: ReactNode }) => <>{children}</>,
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

/** What the browser hands a drop: the dragged type under the React Flow format. */
function dropEvent(type: string, clientX: number, clientY: number) {
  return {
    preventDefault: vi.fn(),
    clientX,
    clientY,
    dataTransfer: {
      getData: vi.fn((format: string) => (format === 'application/reactflow' ? type : '')),
    },
  }
}

const store = () => useFlowWorkflowStore.getState()

beforeEach(() => {
  captured.canvas = undefined
  useFlowWorkflowStore.getState().resetWorkflow()
  toFlow.mockReset()
  toFlow.mockImplementation((position) => position)
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

describe('F-158 dragging a node onto the canvas', () => {
  it('a drag from the palette carries the node type and offers a move', async () => {
    renderEditor()
    await canvas()
    const dataTransfer = { setData: vi.fn(), effectAllowed: 'all' }
    fireEvent.dragStart(screen.getByRole('button', { name: 'Add Price Alert node' }), {
      dataTransfer,
    })
    expect(dataTransfer.setData).toHaveBeenCalledWith('application/reactflow', 'priceAlert')
    expect(dataTransfer.effectAllowed).toBe('move')
  })

  it('the canvas accepts a drag over it as a move', async () => {
    renderEditor()
    const props = await canvas()
    const over = { preventDefault: vi.fn(), dataTransfer: { dropEffect: 'none' } }
    act(() => props.onDragOver?.(over as unknown as React.DragEvent<HTMLDivElement>))
    expect(over.preventDefault).toHaveBeenCalled()
    expect(over.dataTransfer.dropEffect).toBe('move')
  })

  it('a drop adds the node where the pointer is, in flow coordinates, with its defaults', async () => {
    renderEditor()
    const props = await canvas()
    // A canvas panned and zoomed: flow coordinates differ from the screen's.
    toFlow.mockImplementation(({ x, y }) => ({ x: (x - 100) / 2, y: (y - 40) / 2 }))
    const drop = dropEvent('priceAlert', 640, 380)
    act(() => props.onDrop?.(drop as unknown as React.DragEvent<HTMLDivElement>))

    expect(drop.preventDefault).toHaveBeenCalled()
    expect(toFlow).toHaveBeenCalledWith({ x: 640, y: 380 })
    const added = store().nodes.at(-1)
    expect(added).toMatchObject({
      id: 'node_3',
      type: 'priceAlert',
      position: { x: 270, y: 170 },
      data: DEFAULT_NODE_DATA.priceAlert,
    })
    expect(added?.data).not.toBe(DEFAULT_NODE_DATA.priceAlert)
    expect(store().isModified).toBe(true)
  })

  it('a drop of a type without defaults adds the node with empty data', async () => {
    renderEditor()
    const props = await canvas()
    expect(DEFAULT_NODE_DATA).not.toHaveProperty('brokerHoliday')
    act(() =>
      props.onDrop?.(
        dropEvent('brokerHoliday', 300, 200) as unknown as React.DragEvent<HTMLDivElement>
      )
    )
    const added = store().nodes.at(-1)
    expect(added).toMatchObject({ type: 'brokerHoliday', position: { x: 300, y: 200 } })
    expect(added?.data).toEqual({})
  })
})
