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
import { useThemeStore } from '@/stores/themeStore'
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
            <Route path="/positions" element={<p>positions page</p>} />
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
  for (const fn of Object.values(toast)) fn.mockReset()
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
    expect(toast.success).toHaveBeenCalledWith('Workflow saved', 'flow')
  })

  it('a save whose new trigger could not be registered says the workflow stopped (V-151)', async () => {
    api.updateWorkflow.mockImplementation(async (_id: number, data: object) => ({
      ...stored(),
      ...data,
      needs_reactivate: true,
    }))
    renderEditor()
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    await user.click(screen.getByRole('button', { name: /Save/ }))
    await waitFor(() =>
      expect(toast.warning).toHaveBeenCalledWith(
        'Saved, but the new trigger could not be registered, so the workflow was deactivated. Activate it again once the trigger is valid.',
        'flow'
      )
    )
  })

  it('a refused save shows the server reason (V-151)', async () => {
    api.updateWorkflow.mockRejectedValue(new Error('Node "Place Order" is missing a symbol.'))
    renderEditor()
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    await user.click(screen.getByRole('button', { name: /Save/ }))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith('Node "Place Order" is missing a symbol.', 'flow')
    )
    expect(screen.getByText('Unsaved')).toBeInTheDocument()
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

describe('departures from the web editor (desktop)', () => {
  it('a failed run keeps the per-node log its reply carries (F-156)', async () => {
    api.executeWorkflow.mockRejectedValue(
      Object.assign(new Error('Workflow failed at Place Order'), {
        response: {
          status: 500,
          data: {
            status: 'error',
            logs: [
              { time: '2026-10-30T09:20:00', message: 'Getting quote for SBIN', level: 'info' },
              { time: '2026-10-30T09:20:01', message: 'Order rejected: RMS', level: 'error' },
            ],
          },
        },
      })
    )
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    expect(await screen.findByText('Getting quote for SBIN')).toBeInTheDocument()
    expect(screen.getByText('Order rejected: RMS')).toBeInTheDocument()
    expect(screen.getByText('Workflow failed at Place Order')).toBeInTheDocument()
  })

  it('a run that waits says where its orders show, not an execution history (F-207)', async () => {
    api.executeWorkflow.mockResolvedValue({
      status: 'accepted',
      message:
        "Workflow started. It waits on a Delay or Wait Until step, so it finishes in the background; its result appears in the workflow's execution history.",
    })
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    expect(await screen.findByText(/show in the order book/)).toBeInTheDocument()
    expect(screen.queryByText(/execution history/)).not.toBeInTheDocument()
    expect(toast.info.mock.calls[0][0]).not.toMatch(/execution history/)
  })

  it('Replace from JSON reloads the canvas with what was stored (F-169, V-154)', async () => {
    const replaced = stored({
      nodes: [
        ...stored().nodes,
        { id: 'node_3', type: 'log', position: { x: 0, y: 320 }, data: { message: 'done' } },
      ],
    })
    api.replaceWorkflow.mockImplementation(async () => {
      api.getWorkflow.mockResolvedValue(replaced)
      return { status: 'success', workflow_id: 7, needs_reactivate: true }
    })
    const { container } = renderEditor()
    const user = userEvent.setup()
    await loaded()
    await waitFor(() => expect(container.querySelectorAll('.react-flow__node')).toHaveLength(2))
    await user.click(screen.getByRole('button', { name: 'Workflow actions' }))
    await user.click(await screen.findByRole('menuitem', { name: 'Replace from JSON' }))
    const json = await screen.findByPlaceholderText(
      '{ "name": "...", "nodes": [...], "edges": [...] }'
    )
    await user.click(json)
    await user.paste(JSON.stringify({ name: 'ORB', nodes: replaced.nodes, edges: [] }))
    await user.click(screen.getByRole('button', { name: 'Replace' }))

    await waitFor(() => expect(container.querySelectorAll('.react-flow__node')).toHaveLength(3))
    expect(screen.queryByText('Unsaved')).not.toBeInTheDocument()
    expect(toast.warning.mock.calls[0][0]).toBe(
      'Workflow replaced. The trigger changed, so the workflow was deactivated. Activate it again once the trigger is valid.'
    )
  })
})

describe('export and replace (IE-05, IE-16, V-153)', () => {
  async function openMenuItem(name: string) {
    const user = userEvent.setup()
    await loaded()
    await user.click(screen.getByRole('button', { name: 'Workflow actions' }))
    await user.click(await screen.findByRole('menuitem', { name }))
    return user
  }

  it('exports the stored workflow as a JSON file named after it', async () => {
    const exported = {
      version: '1.0',
      name: 'Opening Range #2',
      description: null,
      nodes: [],
      edges: [],
      exported_at: '2026-10-30T09:00:00',
    }
    api.exportWorkflow.mockResolvedValue(exported)
    const createObjectURL = vi.fn((_blob: Blob) => 'blob:flow-export')
    const revokeObjectURL = vi.fn()
    vi.stubGlobal('URL', Object.assign(URL, { createObjectURL, revokeObjectURL }))
    const clicked: HTMLAnchorElement[] = []
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
      this: HTMLAnchorElement
    ) {
      clicked.push(this)
    })
    renderEditor()
    await openMenuItem('Export Workflow')
    await waitFor(() => expect(clicked).toHaveLength(1))
    expect(clicked[0].download).toBe('opening_range__2.json')
    expect(clicked[0].href).toBe('blob:flow-export')
    const blob = createObjectURL.mock.calls[0][0]
    expect(blob.type).toBe('application/json')
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:flow-export')
    expect(toast.success).toHaveBeenCalledWith('Workflow exported', 'flow')
    click.mockRestore()
    vi.unstubAllGlobals()
  })

  it.each([
    ['{not json', 'That is not valid JSON.'],
    ['{"name": "ORB"}', 'Workflow JSON needs a nodes array.'],
  ])('refuses %s before sending it', async (text, sentence) => {
    renderEditor()
    const user = await openMenuItem('Replace from JSON')
    const json = await screen.findByPlaceholderText(
      '{ "name": "...", "nodes": [...], "edges": [...] }'
    )
    await user.click(json)
    await user.paste(text)
    await user.click(screen.getByRole('button', { name: 'Replace' }))
    expect(await screen.findByText(sentence)).toBeInTheDocument()
    expect(api.replaceWorkflow).not.toHaveBeenCalled()
  })

  it("shows the server's reason when it refuses the replacement", async () => {
    api.replaceWorkflow.mockRejectedValue(
      Object.assign(new Error('Request failed'), {
        response: { data: { message: 'Edge e9 points at a node that does not exist.' } },
      })
    )
    renderEditor()
    const user = await openMenuItem('Replace from JSON')
    await user.click(
      await screen.findByPlaceholderText('{ "name": "...", "nodes": [...], "edges": [...] }')
    )
    await user.paste('{"name": "ORB", "nodes": [], "edges": []}')
    await user.click(screen.getByRole('button', { name: 'Replace' }))
    expect(
      await screen.findByText('Edge e9 points at a node that does not exist.')
    ).toBeInTheDocument()
  })
})

describe('the editor header is the app account controls (desktop)', () => {
  it('offers Check for Updates in the profile menu, as every other page does', async () => {
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    expect(screen.getByRole('link', { name: /Dashboard/ })).toHaveAttribute('href', '/dashboard')
    await user.click(screen.getByRole('button', { name: 'Open user menu' }))
    expect(await screen.findByRole('menuitem', { name: 'Check for Updates' })).toBeInTheDocument()
    expect(screen.getByRole('menuitem', { name: 'Logout' })).toBeInTheDocument()
  })

  it('says when the mode switches, as the navbar does', async () => {
    const original = useThemeStore.getState().toggleAppMode
    useThemeStore.setState({
      appMode: 'live',
      toggleAppMode: vi.fn(async () => {
        useThemeStore.setState({ appMode: 'analyzer' })
        return { success: true }
      }),
    })
    renderEditor()
    await loaded()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Switch to Analyze mode' }))
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Switched to Analyze mode'))
    useThemeStore.setState({ appMode: 'live', toggleAppMode: original })
  })

  it('asks before a profile menu entry leaves unsaved changes', async () => {
    renderEditor()
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    await user.click(screen.getByRole('button', { name: 'Open user menu' }))
    await user.click(await screen.findByRole('menuitem', { name: 'Positions' }))
    expect(await screen.findByText('Leave without saving?')).toBeInTheDocument()
    expect(screen.queryByText('positions page')).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Leave without saving' }))
    expect(await screen.findByText('positions page')).toBeInTheDocument()
  })
})
