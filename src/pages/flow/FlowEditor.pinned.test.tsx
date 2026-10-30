/**
 * /flow/editor/:id on a real canvas, pinned where the audit found the
 * behaviour carried from the web but untested: the invalid id, load-failure
 * and loading screens, hydration (the canvas kept across a refetch, blank
 * output names repaired, edges rewritten, node ids, the store reset on
 * leaving), Save, saveIfDirty's bound, Activate and Deactivate, the Run Now
 * reply branches, and the keyboard shortcuts.
 *
 * Pins: F-142 F-143 F-144 F-145 F-146 F-147 F-148 F-149 F-150 F-151 F-152
 * F-154 F-155 F-156 F-157 V-151.
 *
 * The API module and the toasts are mocked as in FlowEditor.test.tsx; a
 * refetch is observed as one more call of the mocked request.
 */

import { QueryClient, QueryClientProvider, useQuery } from '@tanstack/react-query'
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flowQueryKeys, type Workflow } from '@/api/flow'
import { FLOW_STARTED_IN_BACKGROUND } from '@/lib/desktopFlow'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

const api = vi.hoisted(() => ({
  getWorkflow: vi.fn(),
  updateWorkflow: vi.fn(),
  executeWorkflow: vi.fn(),
  activateWorkflow: vi.fn(),
  deactivateWorkflow: vi.fn(),
  exportWorkflow: vi.fn(),
  replaceWorkflow: vi.fn(),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
  getWebhookInfo: vi.fn(),
}))

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return { ...actual, ...api }
})

vi.mock('@/hooks/useProfileMenuItems', () => ({
  useProfileMenuItems: () => [{ href: '/positions', label: 'Positions', icon: () => null }],
}))

const toast = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  info: vi.fn(),
  warning: vi.fn(),
}))
vi.mock('@/utils/toast', () => ({ showToast: toast }))

const listProbe = vi.hoisted(() => vi.fn(async () => []))

const { default: FlowEditor } = await import('./FlowEditor')

const STILL_CHANGING =
  'The canvas kept changing while it was being saved. Stop editing, save, then try again.'

const START = {
  id: 'node_1',
  type: 'start',
  position: { x: 0, y: 0 },
  data: { scheduleType: 'daily', time: '09:20' },
}

const ORDER = {
  id: 'node_2',
  type: 'placeOrder',
  position: { x: 0, y: 160 },
  data: { symbol: 'SBIN', exchange: 'NSE', action: 'BUY', quantity: 1 },
}

function stored(overrides: Partial<Workflow> = {}): Workflow {
  return {
    id: 7,
    name: 'Opening range breakout',
    description: null,
    nodes: [START, ORDER],
    edges: [{ id: 'e1', source: 'node_1', target: 'node_2' }],
    is_active: false,
    schedule_job_id: null,
    webhook_token: null,
    webhook_secret: null,
    webhook_enabled: false,
    webhook_auth_type: 'payload',
    created_at: '2026-10-29T09:00:00',
    updated_at: '2026-10-29T09:00:00',
    ...overrides,
  }
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

/** A second observer of the workflow list, as the /flow page would be. */
function ListProbe() {
  useQuery({ queryKey: flowQueryKeys.workflows(), queryFn: listProbe })
  return null
}

function renderEditor(path = '/flow/editor/7', { withList = false } = {}) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } },
  })
  return render(
    <div style={{ width: 1400, height: 900 }}>
      <MemoryRouter initialEntries={[path]}>
        <QueryClientProvider client={client}>
          {withList && <ListProbe />}
          <Routes>
            <Route path="/flow/editor/:id" element={<FlowEditor />} />
            <Route path="/flow/editor" element={<FlowEditor />} />
            <Route path="/flow/shortcuts" element={<p>shortcuts page</p>} />
            <Route path="/flow" element={<p>workflow list</p>} />
          </Routes>
        </QueryClientProvider>
      </MemoryRouter>
    </div>
  )
}

async function loaded() {
  return screen.findByDisplayValue('Opening range breakout')
}

async function runNow(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: 'Workflow actions' }))
  await user.click(await screen.findByRole('menuitem', { name: 'Run Now' }))
}

const store = () => useFlowWorkflowStore.getState()
const nodeIds = () => store().nodes.map((node) => node.id)

function expectLogoHeader() {
  const logo = screen.getByRole('img', { name: 'OpenAlgo' })
  expect(logo).toHaveAttribute('src', '/images/android-chrome-192x192.png')
  expect(screen.getByText('openalgo')).toBeInTheDocument()
}

beforeEach(() => {
  for (const fn of Object.values(toast)) fn.mockReset()
  for (const fn of Object.values(api)) fn.mockReset()
  listProbe.mockClear()
  api.getIndexSymbolsLotSizes.mockResolvedValue([])
  api.getWorkflow.mockResolvedValue(stored())
  api.updateWorkflow.mockImplementation(async (_id: number, data: object) => ({
    ...stored(),
    ...data,
  }))
  useFlowWorkflowStore.getState().resetWorkflow()
})

describe('F-142 an address that names no workflow', () => {
  it.each([
    ['/flow/editor/abc', 'a word'],
    ['/flow/editor/undefined', 'undefined'],
    ['/flow/editor', 'no id'],
  ])('%s (%s) says the id is invalid and offers the way back', async (path) => {
    renderEditor(path)
    expect(await screen.findByText('Invalid workflow ID')).toBeInTheDocument()
    expectLogoHeader()
    expect(screen.queryByRole('textbox', { name: 'Workflow name' })).not.toBeInTheDocument()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Back to Workflows' }))
    expect(await screen.findByText('workflow list')).toBeInTheDocument()
  })
})

describe('F-143 a workflow that did not load', () => {
  it('says so in its own words when the failure has no message, keeps editing off, and retries', async () => {
    api.getWorkflow.mockRejectedValueOnce('offline')
    const { container } = renderEditor()
    expect(
      await screen.findByRole('heading', { name: 'Could not load this workflow' })
    ).toBeVisible()
    expect(screen.getByText('The workflow could not be loaded.')).toBeInTheDocument()
    expect(
      screen.getByText('Editing is disabled so the stored workflow is not overwritten.')
    ).toBeInTheDocument()
    expectLogoHeader()
    expect(container.querySelector('.react-flow')).toBeNull()
    expect(api.getWorkflow).toHaveBeenCalledTimes(1)

    await userEvent.setup().click(screen.getByRole('button', { name: 'Retry' }))
    expect(await loaded()).toBeInTheDocument()
    expect(api.getWorkflow).toHaveBeenCalledTimes(2)
  })
})

describe('F-144 loading a workflow', () => {
  it('shows the header and a spinner, and no canvas, until the workflow arrives', async () => {
    const workflow = deferred<Workflow>()
    api.getWorkflow.mockReturnValue(workflow.promise)
    const { container } = renderEditor()
    await waitFor(() => expect(api.getWorkflow).toHaveBeenCalledWith(7))
    expectLogoHeader()
    expect(container.querySelector('svg.animate-spin')).not.toBeNull()
    expect(container.querySelector('.react-flow')).toBeNull()
    expect(screen.queryByRole('textbox', { name: 'Workflow name' })).not.toBeInTheDocument()

    workflow.resolve(stored())
    expect(await loaded()).toBeInTheDocument()
    expect(container.querySelector('.react-flow')).not.toBeNull()
  })
})

describe('F-145 the canvas is drawn once per workflow', () => {
  it('a refetch after Activate keeps edits made meanwhile, unsaved, on the canvas', async () => {
    const activated = deferred<{ status: string; message: string }>()
    api.activateWorkflow.mockReturnValue(activated.promise)
    const { container } = renderEditor()
    const user = userEvent.setup()
    const name = await loaded()
    await waitFor(() => expect(container.querySelectorAll('.react-flow__node')).toHaveLength(2))

    await user.click(screen.getByRole('button', { name: 'Activate' }))
    await waitFor(() => expect(api.activateWorkflow).toHaveBeenCalledWith(7))
    api.getWorkflow.mockResolvedValue(
      stored({ name: 'Renamed on the server', nodes: [START], edges: [], is_active: true })
    )
    await user.type(name, ' v2')
    activated.resolve({ status: 'success', message: 'Workflow activated' })

    await waitFor(() => expect(api.getWorkflow).toHaveBeenCalledTimes(2))
    expect(await screen.findByText('Workflow active')).toBeInTheDocument()
    expect(name).toHaveValue('Opening range breakout v2')
    expect(screen.getByText('Unsaved')).toBeInTheDocument()
    expect(nodeIds()).toEqual(['node_1', 'node_2'])
    expect(screen.queryByDisplayValue('Renamed on the server')).not.toBeInTheDocument()
  })

  it('a refetch still updates whether the workflow is active', async () => {
    api.getWorkflow.mockResolvedValue(stored({ is_active: true }))
    api.updateWorkflow.mockImplementation(async (_id: number, data: object) => {
      api.getWorkflow.mockResolvedValue(stored({ name: 'Server copy', is_active: false }))
      return { ...stored(), ...data, needs_reactivate: true }
    })
    renderEditor()
    const user = userEvent.setup()
    const name = await loaded()
    expect(await screen.findByText('Workflow active')).toBeInTheDocument()
    await user.type(name, ' v2')
    await user.click(screen.getByRole('button', { name: /Save/ }))

    await waitFor(() => expect(api.getWorkflow).toHaveBeenCalledTimes(2))
    expect(await screen.findByText('Workflow inactive')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Activate' })).toBeInTheDocument()
    expect(name).toHaveValue('Opening range breakout v2')
  })
})

describe('F-146 what loading does to the stored graph', () => {
  const at = { x: 0, y: 0 }

  it('fills a blank output name from the node default, marks the canvas unsaved, and saves it', async () => {
    api.getWorkflow.mockResolvedValue(
      stored({
        nodes: [
          START,
          { id: 'node_2', type: 'orderBook', position: at, data: { outputVariable: '' } },
          { id: 'node_3', type: 'positionBook', position: at, data: { outputVariable: '   ' } },
          { id: 'node_4', type: 'tradeBook', position: at, data: {} },
          { id: 'node_5', type: 'funds', position: at, data: { outputVariable: 'cash' } },
          {
            id: 'node_6',
            type: 'getQuote',
            position: at,
            data: { symbol: 'SBIN', exchange: 'NSE', outputVariable: '' },
          },
        ],
        edges: [],
      })
    )
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    expect(await screen.findByText('Unsaved')).toBeInTheDocument()
    const save = screen.getByRole('button', { name: /Save/ })
    expect(save).toBeEnabled()
    await user.click(save)

    await waitFor(() => expect(api.updateWorkflow).toHaveBeenCalledTimes(1))
    const names = Object.fromEntries(
      api.updateWorkflow.mock.calls[0][1].nodes.map(
        (node: { id: string; data: { outputVariable?: string } }) => [
          node.id,
          node.data.outputVariable,
        ]
      )
    )
    expect(names).toEqual({
      node_1: undefined,
      node_2: 'orders',
      node_3: 'positions',
      node_4: 'trades',
      node_5: 'cash',
      node_6: '',
    })
  })

  it('leaves a graph with nothing to repair saved', async () => {
    renderEditor()
    await loaded()
    expect(screen.queryByText('Unsaved')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Save/ })).toBeDisabled()
  })

  it('draws every stored edge as an animated insertable edge, keeping its ends', async () => {
    api.getWorkflow.mockResolvedValue(
      stored({
        edges: [
          { id: 'e1', source: 'node_1', target: 'node_2', type: 'default', animated: false },
          { id: 'e2', source: 'node_2', target: 'node_1', sourceHandle: 'true' },
        ],
      })
    )
    renderEditor()
    await loaded()
    await waitFor(() => expect(store().edges).toHaveLength(2))
    expect(store().edges).toEqual([
      { id: 'e1', source: 'node_1', target: 'node_2', type: 'insertable', animated: true },
      {
        id: 'e2',
        source: 'node_2',
        target: 'node_1',
        sourceHandle: 'true',
        type: 'insertable',
        animated: true,
      },
    ])
  })
})

describe('F-147 new node ids', () => {
  it('continue after the highest node_{n} the workflow already has', async () => {
    api.getWorkflow.mockResolvedValue(
      stored({
        nodes: [START, { ...ORDER, id: 'node_7' }, { ...ORDER, id: 'trigger' }],
        edges: [],
      })
    )
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await user.click(screen.getByRole('button', { name: 'Add Price Alert node' }))
    await user.click(screen.getByRole('button', { name: 'Add Price Alert node' }))
    expect(nodeIds()).toEqual(['node_1', 'node_7', 'trigger', 'node_8', 'node_9'])
  })

  it('start at node_1 when no stored id is numbered', async () => {
    api.getWorkflow.mockResolvedValue(
      stored({
        nodes: [
          { ...START, id: 'start' },
          { ...ORDER, id: 'order' },
        ],
        edges: [],
      })
    )
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await user.click(screen.getByRole('button', { name: 'Add Price Alert node' }))
    expect(nodeIds()).toEqual(['start', 'order', 'node_1'])
  })
})

describe('F-148 leaving the editor', () => {
  it('clears the workflow out of the editor store', async () => {
    renderEditor()
    await loaded()
    expect(store().id).toBe(7)
    await userEvent.setup().click(screen.getByRole('button', { name: 'Back to flows list' }))
    expect(await screen.findByText('workflow list')).toBeInTheDocument()
    expect(store()).toMatchObject({
      id: null,
      name: 'Untitled Workflow',
      nodes: [],
      edges: [],
      isModified: false,
    })
  })
})

describe('F-149 V-151 Save', () => {
  it('sends the name and graph without the description, and the list is fetched again', async () => {
    api.getWorkflow.mockResolvedValue(stored({ description: 'NIFTY futures at 09:20' }))
    renderEditor('/flow/editor/7', { withList: true })
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    await waitFor(() => expect(listProbe).toHaveBeenCalledTimes(1))
    await user.click(screen.getByRole('button', { name: /Save/ }))
    await waitFor(() => expect(api.updateWorkflow).toHaveBeenCalledTimes(1))
    expect(Object.keys(api.updateWorkflow.mock.calls[0][1]).sort()).toEqual([
      'edges',
      'name',
      'nodes',
    ])
    await waitFor(() => expect(listProbe).toHaveBeenCalledTimes(2))
    expect(toast.success).toHaveBeenCalledWith('Workflow saved', 'flow')
  })

  it('an edit made while the save is out stays unsaved', async () => {
    const saved = deferred<Workflow>()
    api.updateWorkflow.mockReturnValue(saved.promise)
    renderEditor()
    const user = userEvent.setup()
    const name = await loaded()
    await user.type(name, ' v2')
    await user.click(screen.getByRole('button', { name: /Save/ }))
    await waitFor(() => expect(api.updateWorkflow).toHaveBeenCalledTimes(1))
    await user.type(name, '3')
    saved.resolve(stored({ name: 'Opening range breakout v2' }))
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Workflow saved', 'flow'))
    expect(screen.getByText('Unsaved')).toBeInTheDocument()
  })
})

describe('F-150 V-151 saving before Run Now, Activate and Deactivate', () => {
  it.each([
    ['Activate', false],
    ['Deactivate', true],
    ['Run Now', false],
  ] as const)(
    '%s gives up after three saves of a canvas that keeps changing',
    async (action, active) => {
      api.getWorkflow.mockResolvedValue(stored({ is_active: active }))
      api.updateWorkflow.mockImplementation(async (_id: number, data: { name: string }) => {
        // Someone is still editing: each save is outdated by the time it lands.
        useFlowWorkflowStore.getState().setName(`${data.name}+`)
        return { ...stored(), ...data }
      })
      renderEditor()
      const user = userEvent.setup()
      await user.type(await loaded(), ' v2')
      if (action === 'Run Now') {
        await runNow(user)
      } else {
        await user.click(screen.getByRole('button', { name: action }))
      }
      await waitFor(() => expect(toast.error).toHaveBeenCalledWith(STILL_CHANGING, 'flow'))
      expect(api.updateWorkflow).toHaveBeenCalledTimes(3)
      expect(api.activateWorkflow).not.toHaveBeenCalled()
      expect(api.deactivateWorkflow).not.toHaveBeenCalled()
      expect(api.executeWorkflow).not.toHaveBeenCalled()
    }
  )

  it('saves again when an edit lands during the first save, then acts', async () => {
    let calls = 0
    api.updateWorkflow.mockImplementation(async (_id: number, data: { name: string }) => {
      calls += 1
      if (calls === 1) useFlowWorkflowStore.getState().setName(`${data.name}+`)
      return { ...stored(), ...data }
    })
    api.activateWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    renderEditor()
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    await user.click(screen.getByRole('button', { name: 'Activate' }))
    await waitFor(() => expect(api.activateWorkflow).toHaveBeenCalledWith(7))
    expect(api.updateWorkflow).toHaveBeenCalledTimes(2)
    expect(api.updateWorkflow.mock.calls[1][1].name).toBe('Opening range breakout v2+')
    expect(toast.error).not.toHaveBeenCalled()
  })
})

describe('F-151 Activate', () => {
  it('says the workflow is active and fetches it again', async () => {
    api.activateWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    renderEditor()
    await loaded()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Activate' }))
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Workflow activated', 'flow'))
    await waitFor(() => expect(api.getWorkflow).toHaveBeenCalledTimes(2))
    expect(screen.getByText('Workflow active')).toBeInTheDocument()
  })

  it("shows the server's reason and stays inactive when it refuses", async () => {
    api.activateWorkflow.mockRejectedValue(new Error('Add a trigger before activating.'))
    renderEditor()
    await loaded()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Activate' }))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith('Add a trigger before activating.', 'flow')
    )
    expect(toast.success).not.toHaveBeenCalled()
    expect(screen.getByText('Workflow inactive')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Activate' })).toBeInTheDocument()
  })
})

describe('F-152 Deactivate', () => {
  it('says the workflow is stopped and fetches it again', async () => {
    api.getWorkflow.mockResolvedValue(stored({ is_active: true }))
    api.deactivateWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    renderEditor()
    await loaded()
    await userEvent.setup().click(await screen.findByRole('button', { name: 'Deactivate' }))
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Workflow deactivated', 'flow'))
    await waitFor(() => expect(api.getWorkflow).toHaveBeenCalledTimes(2))
  })

  it("shows the server's reason and stays active when it refuses", async () => {
    api.getWorkflow.mockResolvedValue(stored({ is_active: true }))
    api.deactivateWorkflow.mockRejectedValue(new Error('The workflow is running. Try again.'))
    renderEditor()
    await loaded()
    await userEvent.setup().click(await screen.findByRole('button', { name: 'Deactivate' }))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith('The workflow is running. Try again.', 'flow')
    )
    expect(screen.getByText('Workflow active')).toBeInTheDocument()
  })
})

describe('F-154 a run started in the background', () => {
  it('shows the run as running in the background, with the one sentence in the log and the toast', async () => {
    api.executeWorkflow.mockResolvedValue({ status: 'accepted', message: 'started' })
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    expect(await screen.findByText('Running in background')).toBeInTheDocument()
    expect(screen.getByText(FLOW_STARTED_IN_BACKGROUND)).toBeInTheDocument()
    expect(toast.info).toHaveBeenCalledWith(FLOW_STARTED_IN_BACKGROUND, 'flow')
    expect(toast.success).not.toHaveBeenCalled()
    expect(toast.error).not.toHaveBeenCalled()
  })
})

describe('F-155 a run the server answered', () => {
  it("a success shows the server's message, the log and Completed", async () => {
    api.executeWorkflow.mockResolvedValue({
      status: 'success',
      message: 'Workflow executed successfully',
      logs: [{ time: '2026-10-30T09:20:00', message: 'Order placed: SBIN', level: 'info' }],
    })
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    expect(await screen.findByText('Completed')).toBeInTheDocument()
    expect(screen.getByText('Order placed: SBIN')).toBeInTheDocument()
    expect(toast.success).toHaveBeenCalledWith('Workflow executed successfully', 'flow')
  })

  it('a success without a message says Execution completed', async () => {
    api.executeWorkflow.mockResolvedValue({ status: 'success', message: '' })
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Execution completed', 'flow'))
  })

  it("any other status shows the server's message, the log and Failed", async () => {
    api.executeWorkflow.mockResolvedValue({
      status: 'error',
      message: 'Order rejected: RMS',
      logs: [{ time: '2026-10-30T09:20:00', message: 'Placing order', level: 'info' }],
    })
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    expect(await screen.findByText('Failed')).toBeInTheDocument()
    expect(screen.getByText('Placing order')).toBeInTheDocument()
    expect(toast.error).toHaveBeenCalledWith('Order rejected: RMS', 'flow')
    expect(toast.success).not.toHaveBeenCalled()
  })

  it('any other status without a message says Execution failed', async () => {
    api.executeWorkflow.mockResolvedValue({ status: 'error', message: '' })
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Execution failed', 'flow'))
  })
})

describe('F-156 a run request that failed', () => {
  it('shows Failed, the reason in the log, and the reason in a toast', async () => {
    api.executeWorkflow.mockRejectedValue(new Error('Another run of this workflow is in progress.'))
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    expect(await screen.findByText('Failed')).toBeInTheDocument()
    expect(screen.getByText('Another run of this workflow is in progress.')).toBeInTheDocument()
    expect(toast.error).toHaveBeenCalledWith('Another run of this workflow is in progress.', 'flow')
  })
})

describe('F-157 keyboard shortcuts', () => {
  it('Delete removes the selected node with its edges', async () => {
    renderEditor()
    await loaded()
    act(() => store().selectNode('node_2'))
    const notPrevented = fireEvent.keyDown(window, { key: 'Delete' })
    expect(notPrevented).toBe(false)
    expect(nodeIds()).toEqual(['node_1'])
    expect(store().edges).toEqual([])
    expect(await screen.findByText('Unsaved')).toBeInTheDocument()
  })

  it('Backspace removes the selected edge and leaves the nodes', async () => {
    renderEditor()
    await loaded()
    act(() => store().selectEdge('e1'))
    const notPrevented = fireEvent.keyDown(window, { key: 'Backspace' })
    expect(notPrevented).toBe(false)
    expect(store().edges).toEqual([])
    expect(nodeIds()).toEqual(['node_1', 'node_2'])
  })

  it('Delete with nothing selected does nothing and keeps the key', async () => {
    renderEditor()
    await loaded()
    const notPrevented = fireEvent.keyDown(window, { key: 'Delete' })
    expect(notPrevented).toBe(true)
    expect(nodeIds()).toEqual(['node_1', 'node_2'])
    expect(screen.queryByText('Unsaved')).not.toBeInTheDocument()
  })

  it('no shortcut fires while typing in a text box or a text area', async () => {
    renderEditor()
    const name = await loaded()
    const notes = document.createElement('textarea')
    document.body.append(notes)
    try {
      act(() => store().selectNode('node_2'))
      fireEvent.keyDown(name, { key: 'Delete' })
      fireEvent.keyDown(notes, { key: 'Backspace' })
      fireEvent.keyDown(name, { key: 'Escape' })
      expect(nodeIds()).toEqual(['node_1', 'node_2'])
      expect(store().selectedNodeId).toBe('node_2')

      act(() => store().setName('ORB v2'))
      expect(fireEvent.keyDown(name, { key: 's', ctrlKey: true })).toBe(true)
      expect(fireEvent.keyDown(notes, { key: 's', metaKey: true })).toBe(true)
      fireEvent.keyDown(name, { key: '?' })
      expect(api.updateWorkflow).not.toHaveBeenCalled()
      expect(screen.queryByText('shortcuts page')).not.toBeInTheDocument()
    } finally {
      notes.remove()
    }
  })

  it.each([
    ['Ctrl+S', { ctrlKey: true }],
    ['Cmd+S', { metaKey: true }],
  ])('%s saves an edited canvas', async (_label, modifier) => {
    renderEditor()
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    const notPrevented = fireEvent.keyDown(window, { key: 's', ...modifier })
    expect(notPrevented).toBe(false)
    await waitFor(() => expect(api.updateWorkflow).toHaveBeenCalledTimes(1))
    expect(api.updateWorkflow.mock.calls[0][1].name).toBe('Opening range breakout v2')
  })

  it('Ctrl+S on a saved canvas sends nothing but still keeps the browser from saving the page', async () => {
    renderEditor()
    await loaded()
    expect(fireEvent.keyDown(window, { key: 's', ctrlKey: true })).toBe(false)
    expect(api.updateWorkflow).not.toHaveBeenCalled()
  })

  it('Ctrl+S while a save is out does not send a second one', async () => {
    const saved = deferred<Workflow>()
    api.updateWorkflow.mockReturnValue(saved.promise)
    renderEditor()
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    fireEvent.keyDown(window, { key: 's', ctrlKey: true })
    await waitFor(() => expect(api.updateWorkflow).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(screen.getByRole('button', { name: /Save/ })).toBeDisabled())
    fireEvent.keyDown(window, { key: 's', ctrlKey: true })
    // A second save would start its request a few ticks after the key, so
    // count only once the first save has landed.
    saved.resolve(stored({ name: 'Opening range breakout v2' }))
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Workflow saved', 'flow'))
    expect(api.updateWorkflow).toHaveBeenCalledTimes(1)
  })

  it('Escape clears the selected node, closing its settings, and the selected edge', async () => {
    renderEditor()
    await loaded()
    act(() => store().selectNode('node_2'))
    expect(await screen.findByRole('heading', { level: 2, name: 'Place Order' })).toBeVisible()
    fireEvent.keyDown(window, { key: 'Escape' })
    expect(store().selectedNodeId).toBeNull()
    await waitFor(() =>
      expect(
        screen.queryByRole('heading', { level: 2, name: 'Place Order' })
      ).not.toBeInTheDocument()
    )

    act(() => store().selectEdge('e1'))
    fireEvent.keyDown(window, { key: 'Escape' })
    expect(store().selectedEdgeId).toBeNull()
  })

  it('Shift+/ opens the shortcuts page', async () => {
    renderEditor()
    await loaded()
    fireEvent.keyDown(window, { key: '/', shiftKey: true })
    expect(await screen.findByText('shortcuts page')).toBeInTheDocument()
  })
})
