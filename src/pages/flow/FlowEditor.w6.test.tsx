/**
 * /flow/editor/:id on a real canvas: the details the wave 6 audit found still
 * unpinned after FlowEditor.pinned.test.tsx and FlowEditorMenu.pinned.test.tsx.
 * No request for an address without an id, Run Now's running state and the
 * cleared log, the background-start line word for word, Delete inside an
 * editable element, the status panel's colours, which side panel wins, the
 * header's account controls, and the Replace reply as the server sends it.
 *
 * Pins: F-141 F-153 F-154 F-157 F-162 F-163 F-164 F-215.
 *
 * The harness is FlowEditor.pinned.test.tsx's: the API module and the toasts
 * mocked, a refetch observed as one more call of the mocked request.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ReplaceWorkflowResult, Workflow } from '@/api/flow'
import { useAuthStore } from '@/stores/authStore'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'
import { useThemeStore } from '@/stores/themeStore'

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

const logoutRequest = vi.hoisted(() => vi.fn())
vi.mock('@/api/auth', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/auth')>()
  return { ...actual, authApi: { ...actual.authApi, logout: logoutRequest } }
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

/** The desktop's sentence for a run that waits, as the trader reads it (F-207). */
const STARTED_IN_BACKGROUND =
  'Workflow started. It waits on a Delay or Wait Until step and finishes in the background: its orders go out when the wait ends and show in the order book.'

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

function renderEditor(path = '/flow/editor/7') {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } },
  })
  return render(
    <div style={{ width: 1400, height: 900 }}>
      <MemoryRouter initialEntries={[path]}>
        <QueryClientProvider client={client}>
          <Routes>
            <Route path="/flow/editor/:id" element={<FlowEditor />} />
            <Route path="/flow/editor" element={<FlowEditor />} />
            <Route path="/flow/shortcuts" element={<p>shortcuts page</p>} />
            <Route path="/flow" element={<p>workflow list</p>} />
            <Route path="/login" element={<p>login page</p>} />
          </Routes>
        </QueryClientProvider>
      </MemoryRouter>
    </div>
  )
}

async function loaded() {
  return screen.findByDisplayValue('Opening range breakout')
}

async function openMenu(user: ReturnType<typeof userEvent.setup>) {
  await user.click(await screen.findByRole('button', { name: 'Workflow actions' }))
  return screen.findByRole('menu')
}

async function runNow(user: ReturnType<typeof userEvent.setup>) {
  await openMenu(user)
  await user.click(await screen.findByRole('menuitem', { name: 'Run Now' }))
}

function iconOf(element: Element | null | undefined) {
  return element?.getAttribute('class') ?? ''
}

const store = () => useFlowWorkflowStore.getState()
const nodeIds = () => store().nodes.map((node) => node.id)

beforeEach(() => {
  for (const fn of Object.values(toast)) fn.mockReset()
  for (const fn of Object.values(api)) fn.mockReset()
  logoutRequest.mockReset()
  api.getIndexSymbolsLotSizes.mockResolvedValue([])
  api.getWorkflow.mockResolvedValue(stored())
  api.updateWorkflow.mockImplementation(async (_id: number, data: object) => ({
    ...stored(),
    ...data,
  }))
  useFlowWorkflowStore.getState().resetWorkflow()
})

afterEach(() => {
  useThemeStore.setState({ appMode: 'live', mode: 'light' })
  useAuthStore.setState({ user: null, isAuthenticated: false })
})

describe('F-141 an address without a workflow id', () => {
  it('asks the server for nothing', async () => {
    renderEditor('/flow/editor')
    expect(await screen.findByText('Invalid workflow ID')).toBeInTheDocument()
    // A query that ran would have called the request by now; give it a few ticks.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20))
    })
    expect(api.getWorkflow).not.toHaveBeenCalled()
  })
})

describe('F-153 Run Now while the request is out', () => {
  it("shows Executing..., with the previous run's log cleared", async () => {
    api.executeWorkflow.mockResolvedValueOnce({
      status: 'success',
      message: 'Workflow executed',
      logs: [{ time: '2026-10-29T06:30:00Z', message: 'Order placed: SBIN', level: 'info' }],
    })
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await runNow(user)
    expect(await screen.findByText('Order placed: SBIN')).toBeInTheDocument()
    expect(screen.getByText('Completed')).toBeInTheDocument()

    const second = deferred<{ status: string; message: string; logs: unknown[] }>()
    api.executeWorkflow.mockReturnValueOnce(second.promise)
    await runNow(user)
    await waitFor(() => expect(api.executeWorkflow).toHaveBeenCalledTimes(2))
    expect(await screen.findByText('Executing...')).toBeInTheDocument()
    expect(screen.queryByText('Completed')).not.toBeInTheDocument()
    expect(screen.queryByText('Order placed: SBIN')).not.toBeInTheDocument()
    expect(screen.getByText('No logs yet')).toBeInTheDocument()

    second.resolve({
      status: 'success',
      message: 'Workflow executed',
      logs: [{ time: '2026-10-29T06:31:00Z', message: 'Order placed: INFY', level: 'info' }],
    })
    expect(await screen.findByText('Order placed: INFY')).toBeInTheDocument()
    expect(screen.queryByText('Order placed: SBIN')).not.toBeInTheDocument()
  }, 15000)
})

describe('F-154 a run started in the background', () => {
  it('says so in these words, in the toast and as one info line of the log', async () => {
    api.executeWorkflow.mockResolvedValue({ status: 'accepted', message: 'started' })
    renderEditor()
    await loaded()
    await runNow(userEvent.setup())
    expect(await screen.findByText('Running in background')).toBeInTheDocument()
    expect(toast.info).toHaveBeenCalledWith(STARTED_IN_BACKGROUND, 'flow')

    const line = screen.getByText(STARTED_IN_BACKGROUND)
    // The log row: an info line is neither the red error nor the amber warning.
    const row = line.closest('div.rounded-lg.border')
    expect(row).toHaveClass('border-border', 'bg-background')
    expect(row).not.toHaveClass('border-red-500/30')
    expect(row).not.toHaveClass('border-amber-500/30')
    expect(line).not.toHaveClass('text-red-500')
    expect(iconOf(row?.querySelector('svg'))).toContain('lucide-circle-check')
    expect(screen.queryByText('started')).not.toBeInTheDocument()
    expect(screen.queryByText('No logs yet')).not.toBeInTheDocument()
  })
})

describe('F-157 keyboard shortcuts inside an editable element', () => {
  it('Delete and Escape typed in a contentEditable box leave the selection alone', async () => {
    renderEditor()
    await loaded()
    const note = document.createElement('div')
    // jsdom computes no isContentEditable; the webview does.
    Object.defineProperty(note, 'isContentEditable', { value: true })
    document.body.append(note)
    try {
      act(() => store().selectNode('node_2'))
      expect(fireEvent.keyDown(note, { key: 'Delete' })).toBe(true)
      fireEvent.keyDown(note, { key: 'Backspace' })
      fireEvent.keyDown(note, { key: 'Escape' })
      expect(nodeIds()).toEqual(['node_1', 'node_2'])
      expect(store().edges).toHaveLength(1)
      expect(store().selectedNodeId).toBe('node_2')
      expect(screen.queryByText('Unsaved')).not.toBeInTheDocument()
    } finally {
      note.remove()
    }
  })
})

describe('F-162 the status panel', () => {
  function panelOf(text: string) {
    const label = screen.getByText(text)
    return { panel: label.parentElement, dot: label.previousElementSibling }
  }

  it('is tinted green with a green dot while the workflow is active', async () => {
    api.getWorkflow.mockResolvedValue(stored({ is_active: true }))
    renderEditor()
    await loaded()
    await screen.findByText('Workflow active')
    const { panel, dot } = panelOf('Workflow active')
    expect(panel).toHaveClass('border-green-500/30', 'bg-green-500/5')
    expect(dot).toHaveClass('h-2', 'w-2', 'rounded-full', 'bg-green-500')
    expect(dot).not.toHaveClass('bg-muted-foreground')
  })

  it('is plain with a grey dot while the workflow is inactive', async () => {
    renderEditor()
    await loaded()
    const { panel, dot } = panelOf('Workflow inactive')
    expect(panel).toHaveClass('border-border', 'bg-card')
    expect(panel).not.toHaveClass('border-green-500/30')
    expect(panel).not.toHaveClass('bg-green-500/5')
    expect(dot).toHaveClass('bg-muted-foreground')
    expect(dot).not.toHaveClass('bg-green-500')
  })
})

describe('F-163 the right-hand panel', () => {
  it("a selected node's settings take the place of the open log, which returns once nothing is selected", async () => {
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await openMenu(user)
    await user.click(await screen.findByRole('menuitem', { name: 'Show Logs' }))
    expect(await screen.findByText('Execution Log')).toBeInTheDocument()

    act(() => store().selectNode('node_2'))
    expect(await screen.findByRole('heading', { level: 2, name: 'Place Order' })).toBeVisible()
    expect(screen.queryByText('Execution Log')).not.toBeInTheDocument()

    fireEvent.keyDown(window, { key: 'Escape' })
    expect(await screen.findByText('Execution Log')).toBeInTheDocument()
    expect(screen.queryByRole('heading', { level: 2, name: 'Place Order' })).not.toBeInTheDocument()
  })

  it("the log's own close button closes it, and the menu offers Show Logs again", async () => {
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await openMenu(user)
    await user.click(await screen.findByRole('menuitem', { name: 'Show Logs' }))
    expect(await screen.findByText('Execution Log')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Close execution log' }))
    await waitFor(() => expect(screen.queryByText('Execution Log')).not.toBeInTheDocument())
    const menu = await openMenu(user)
    expect(within(menu).getByRole('menuitem', { name: 'Show Logs' })).toBeInTheDocument()
    expect(within(menu).queryByRole('menuitem', { name: 'Hide Logs' })).not.toBeInTheDocument()
  })
})

describe('F-164 the account controls in the editor header', () => {
  function modeBadge(label: string) {
    return screen.getByText(label).closest('[data-slot="badge"]')
  }

  it('shows Live Mode on the plain badge in live mode, and Analyze Mode in purple in analyzer mode', async () => {
    useThemeStore.setState({ appMode: 'live', mode: 'light' })
    const { unmount } = renderEditor()
    await loaded()
    const live = modeBadge('Live Mode')
    expect(live).toHaveTextContent('Live ModeLive')
    expect(live).not.toHaveClass('bg-purple-500')
    expect(screen.queryByText('Analyze Mode')).not.toBeInTheDocument()
    unmount()

    useThemeStore.setState({ appMode: 'analyzer' })
    renderEditor()
    await loaded()
    const analyzer = modeBadge('Analyze Mode')
    expect(analyzer).toHaveTextContent('Analyze ModeAnalyze')
    expect(analyzer).toHaveClass('bg-purple-500', 'text-white')
    expect(screen.queryByText('Live Mode')).not.toBeInTheDocument()
  })

  it('lets the theme change in live mode only, naming the theme it switches to', async () => {
    useThemeStore.setState({ appMode: 'live', mode: 'light' })
    const { unmount } = renderEditor()
    const user = userEvent.setup()
    await loaded()
    const toDark = screen.getByRole('button', { name: 'Switch to dark mode' })
    expect(toDark).toBeEnabled()
    expect(toDark).toHaveAttribute('title', 'Switch to dark mode')
    await user.click(toDark)
    expect(useThemeStore.getState().mode).toBe('dark')
    expect(screen.getByRole('button', { name: 'Switch to light mode' })).toBeEnabled()
    unmount()

    useThemeStore.setState({ appMode: 'analyzer', mode: 'dark' })
    renderEditor()
    await loaded()
    const locked = screen.getByRole('button', { name: 'Switch to light mode' })
    expect(locked).toBeDisabled()
    expect(useThemeStore.getState().mode).toBe('dark')
  })

  it('offers the Docs in a new window from the profile menu', async () => {
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await user.click(screen.getByRole('button', { name: 'Open user menu' }))
    const docs = await screen.findByRole('menuitem', { name: 'Docs' })
    expect(docs.tagName).toBe('A')
    expect(docs).toHaveAttribute('href', 'https://docs.openalgo.in')
    expect(docs).toHaveAttribute('target', '_blank')
    expect(docs).toHaveAttribute('rel', 'noopener noreferrer')
  })

  it('Logout asks first, then ends the session and opens the login page', async () => {
    logoutRequest.mockResolvedValue(undefined)
    useAuthStore.setState({
      user: {
        username: 'trader',
        broker: 'zerodha',
        isLoggedIn: true,
        loginTime: '2026-10-29T06:30:00.000Z',
      },
      isAuthenticated: true,
    })
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await user.click(screen.getByRole('button', { name: 'Open user menu' }))
    await user.click(await screen.findByRole('menuitem', { name: 'Logout' }))
    const confirm = await screen.findByRole('dialog', { name: 'Confirm Logout' })
    expect(logoutRequest).not.toHaveBeenCalled()
    expect(useAuthStore.getState().isAuthenticated).toBe(true)

    await user.click(within(confirm).getByRole('button', { name: 'Yes, Logout' }))
    expect(await screen.findByText('login page')).toBeInTheDocument()
    expect(logoutRequest).toHaveBeenCalledTimes(1)
    expect(useAuthStore.getState()).toMatchObject({ user: null, isAuthenticated: false })
    expect(toast.success).toHaveBeenCalledWith('Logged out successfully')
  })
})

describe('F-215 the Replace reply as the server sends it', () => {
  async function replaceWith(reply: ReplaceWorkflowResult) {
    api.replaceWorkflow.mockResolvedValue(reply)
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await openMenu(user)
    await user.click(await screen.findByRole('menuitem', { name: 'Replace from JSON' }))
    const dialog = await screen.findByRole('dialog', { name: 'Replace from JSON' })
    await user.click(
      within(dialog).getByPlaceholderText('{ "name": "...", "nodes": [...], "edges": [...] }')
    )
    await user.paste('{"name": "ORB", "nodes": [], "edges": []}')
    await user.click(within(dialog).getByRole('button', { name: 'Replace' }))
  }

  it('an empty migrations list and needs_reactivate false say only Workflow replaced.', async () => {
    await replaceWith({
      status: 'success',
      workflow_id: 7,
      migrations: [],
      needs_reactivate: false,
    })
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Workflow replaced.', 'flow'))
    expect(toast.warning).not.toHaveBeenCalled()
  })

  it('needs_reactivate with an empty migrations list warns without a count', async () => {
    await replaceWith({
      status: 'success',
      workflow_id: 7,
      migrations: [],
      needs_reactivate: true,
    })
    await waitFor(() =>
      expect(toast.warning).toHaveBeenCalledWith(
        'Workflow replaced. The trigger changed, so the workflow was deactivated. Activate it again once the trigger is valid.',
        'flow'
      )
    )
    expect(toast.success).not.toHaveBeenCalled()
  })
})
