/**
 * /flow/editor/:id on a real canvas: loading a stored workflow, editing it,
 * and the rule the web learned the hard way, that Run Now and Activate act on
 * the graph on screen (a dirty canvas is saved first). The API module is
 * mocked as the web's tests mock it; api/flow.test.ts pins the wire.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { Workflow } from '@/api/flow'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'
import { checkA11y } from '@/test/a11y-utils'

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

vi.mock('@/hooks/useProfileMenuItems', () => ({ useProfileMenuItems: () => [] }))

const { default: FlowEditor } = await import('./FlowEditor')

function stored(overrides: Partial<Workflow> = {}): Workflow {
  return {
    id: 7,
    name: 'Opening range breakout',
    description: null,
    nodes: [
      {
        id: 'node_1',
        type: 'start',
        position: { x: 0, y: 0 },
        data: { scheduleType: 'daily', time: '09:20' },
      },
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
    ...overrides,
  }
}

function renderEditor() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } },
  })
  return render(
    <div style={{ width: 1400, height: 900 }}>
      <MemoryRouter initialEntries={['/flow/editor/7']}>
        <QueryClientProvider client={client}>
          <Routes>
            <Route path="/flow/editor/:id" element={<FlowEditor />} />
            <Route path="/flow/shortcuts" element={<p>shortcuts page</p>} />
            <Route path="/flow" element={<p>workflow list</p>} />
            <Route path="/dashboard" element={<p>dashboard page</p>} />
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

beforeEach(() => {
  for (const fn of Object.values(api)) fn.mockReset()
  api.getIndexSymbolsLotSizes.mockResolvedValue([])
  api.getWorkflow.mockResolvedValue(stored())
  api.updateWorkflow.mockImplementation(async (_id: number, data: object) => ({
    ...stored(),
    ...data,
  }))
  useFlowWorkflowStore.getState().resetWorkflow()
})

describe('opening a workflow', () => {
  it('draws the stored graph, saved, with its status', async () => {
    const { container } = renderEditor()
    await loaded()
    await waitFor(() => expect(container.querySelectorAll('.react-flow__node')).toHaveLength(2))
    expect(screen.getByText('Workflow inactive')).toBeInTheDocument()
    expect(screen.queryByText('Unsaved')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Save/ })).toBeDisabled()
    expect(api.getWorkflow).toHaveBeenCalledWith(7)
  })

  it('refuses to edit a workflow that did not load, so nothing overwrites it', async () => {
    api.getWorkflow.mockRejectedValue(new Error('This workflow no longer exists.'))
    renderEditor()
    expect(await screen.findByText('Could not load this workflow')).toBeInTheDocument()
    expect(screen.getByText('This workflow no longer exists.')).toBeInTheDocument()
    expect(screen.queryByDisplayValue('Opening range breakout')).not.toBeInTheDocument()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Back to workflows' }))
    expect(await screen.findByText('workflow list')).toBeInTheDocument()
  })

  it('passes the accessibility checks', async () => {
    const { container } = renderEditor()
    await loaded()
    await checkA11y(container)
  })
})

describe('editing and saving', () => {
  it('a rename marks the workflow unsaved and Save sends the graph', async () => {
    renderEditor()
    const user = userEvent.setup()
    const name = await loaded()
    await user.clear(name)
    await user.type(name, 'ORB 09:20')
    expect(screen.getByText('Unsaved')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: /Save/ }))
    await waitFor(() => expect(api.updateWorkflow).toHaveBeenCalledTimes(1))
    const [id, body] = api.updateWorkflow.mock.calls[0]
    expect(id).toBe(7)
    expect(body.name).toBe('ORB 09:20')
    expect(body.nodes.map((n: { id: string }) => n.id)).toEqual(['node_1', 'node_2'])
    expect(body.edges.map((e: { id: string }) => e.id)).toEqual(['e1'])
    await waitFor(() => expect(screen.queryByText('Unsaved')).not.toBeInTheDocument())
  })

  it('adds a node from the palette without a pointer and opens its config', async () => {
    const { container } = renderEditor()
    const user = userEvent.setup()
    await loaded()
    await user.click(screen.getByRole('button', { name: 'Add Price Alert node' }))
    await waitFor(() => expect(container.querySelectorAll('.react-flow__node')).toHaveLength(3))
    expect(screen.getByRole('heading', { level: 2, name: 'Price Alert' })).toBeInTheDocument()
    expect(screen.getByText('Unsaved')).toBeInTheDocument()
  })

  it('? opens the shortcuts page', async () => {
    renderEditor()
    await loaded()
    fireEvent.keyDown(window, { key: '?' })
    expect(await screen.findByText('shortcuts page')).toBeInTheDocument()
  })
})

describe('Run Now and Activate act on the graph on screen', () => {
  it('Run Now on a saved canvas runs it and shows the log', async () => {
    api.executeWorkflow.mockResolvedValue({
      status: 'success',
      message: 'Workflow executed',
      logs: [{ time: '2026-10-30T09:20:00', message: 'Order placed: SBIN', level: 'info' }],
    })
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await runNow(user)
    await waitFor(() => expect(api.executeWorkflow).toHaveBeenCalledWith(7))
    expect(api.updateWorkflow).not.toHaveBeenCalled()
    expect(await screen.findByText('Order placed: SBIN')).toBeInTheDocument()
  })

  it('Run Now on an edited canvas saves it first', async () => {
    const order: string[] = []
    api.updateWorkflow.mockImplementation(async (_id: number, data: object) => {
      order.push('save')
      return { ...stored(), ...data }
    })
    api.executeWorkflow.mockImplementation(async () => {
      order.push('run')
      return { status: 'success', message: 'Workflow executed', logs: [] }
    })
    renderEditor()
    const user = userEvent.setup()
    const name = await loaded()
    await user.type(name, ' v2')
    await runNow(user)
    await waitFor(() => expect(order).toEqual(['save', 'run']))
    expect(api.updateWorkflow.mock.calls[0][1].name).toBe('Opening range breakout v2')
  })

  it('a run that fails shows why in the log', async () => {
    api.executeWorkflow.mockRejectedValue(new Error('Broker session expired. Log in again.'))
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    expect(await screen.findByText('Broker session expired. Log in again.')).toBeInTheDocument()
  })

  it('Activate saves an edited canvas, then activates it', async () => {
    api.activateWorkflow.mockResolvedValue({ status: 'success', message: 'Workflow activated' })
    renderEditor()
    const user = userEvent.setup()
    const name = await loaded()
    await user.type(name, ' v2')
    await user.click(screen.getByRole('button', { name: 'Activate' }))
    await waitFor(() => expect(api.activateWorkflow).toHaveBeenCalledWith(7))
    expect(api.updateWorkflow).toHaveBeenCalledTimes(1)
    expect(await screen.findByText('Workflow active')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Deactivate' })).toBeInTheDocument()
  })

  it('an active workflow offers Deactivate', async () => {
    api.getWorkflow.mockResolvedValue(stored({ is_active: true }))
    api.deactivateWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    expect(await screen.findByText('Workflow active')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Deactivate' }))
    await waitFor(() => expect(api.deactivateWorkflow).toHaveBeenCalledWith(7))
    expect(await screen.findByText('Workflow inactive')).toBeInTheDocument()
  })
})

describe('leaving with unsaved changes (desktop)', () => {
  async function editName() {
    const user = userEvent.setup()
    const name = await loaded()
    await user.type(name, ' v2')
    expect(screen.getByText('Unsaved')).toBeInTheDocument()
    return user
  }

  it('leaves a saved workflow without asking', async () => {
    renderEditor()
    await loaded()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Back to flows list' }))
    expect(await screen.findByText('workflow list')).toBeInTheDocument()
  })

  it('asks before the back arrow discards edits, and Stay keeps them', async () => {
    renderEditor()
    const user = await editName()
    await user.click(screen.getByRole('button', { name: 'Back to flows list' }))
    expect(await screen.findByText('Leave without saving?')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Stay' }))
    await waitFor(() => expect(screen.queryByText('Leave without saving?')).not.toBeInTheDocument())
    expect(screen.getByDisplayValue('Opening range breakout v2')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Back to flows list' }))
    await user.click(await screen.findByRole('button', { name: 'Leave without saving' }))
    expect(await screen.findByText('workflow list')).toBeInTheDocument()
    expect(api.updateWorkflow).not.toHaveBeenCalled()
  })

  it('asks before the ? key leaves', async () => {
    renderEditor()
    const user = await editName()
    // Typing into the name box is not a shortcut, so leave the field first.
    ;(document.activeElement as HTMLElement | null)?.blur()
    fireEvent.keyDown(window, { key: '?' })
    await user.click(await screen.findByRole('button', { name: 'Leave without saving' }))
    expect(await screen.findByText('shortcuts page')).toBeInTheDocument()
  })

  it('asks before a link in the header leaves', async () => {
    renderEditor()
    const user = await editName()
    await user.click(screen.getByRole('link', { name: /Dashboard/ }))
    expect(await screen.findByText('Leave without saving?')).toBeInTheDocument()
    expect(screen.queryByText('dashboard page')).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Leave without saving' }))
    expect(await screen.findByText('dashboard page')).toBeInTheDocument()
  })

  it('asks the window to confirm a reload or close only while there are edits', async () => {
    renderEditor()
    await loaded()
    const clean = new Event('beforeunload', { cancelable: true })
    window.dispatchEvent(clean)
    expect(clean.defaultPrevented).toBe(false)

    await editName()
    const dirty = new Event('beforeunload', { cancelable: true })
    window.dispatchEvent(dirty)
    expect(dirty.defaultPrevented).toBe(true)
  })
})
