/**
 * /flow, the workflow list, pinned where the audit found the behaviour carried
 * from the web but untested: the loading, error and empty states, the cards,
 * the actions menu with its toasts and list refetches, the create and import
 * dialogs, and the webhook settings dialog.
 *
 * Pins: F-110 F-111 F-112 F-113 F-114 F-115 F-116 F-117 F-118 F-119 F-120
 * F-121 F-122 F-123 F-125 F-130 F-131 F-132 F-133 F-134 F-135 F-136 F-137
 * F-139 IE-09 V-152.
 *
 * The API module and the toasts are mocked as in FlowIndex.test.tsx and
 * FlowEditor.test.tsx; a refetch is observed as one more call of the mocked
 * request.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes, useLocation, useParams } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { WebhookInfo, WorkflowListItem } from '@/api/flow'

const api = vi.hoisted(() => ({
  listWorkflows: vi.fn(),
  createWorkflow: vi.fn(),
  importWorkflow: vi.fn(),
  activateWorkflow: vi.fn(),
  deactivateWorkflow: vi.fn(),
  deleteWorkflow: vi.fn(),
  getWebhookInfo: vi.fn(),
  enableWebhook: vi.fn(),
  disableWebhook: vi.fn(),
  regenerateWebhook: vi.fn(),
  updateWebhookAuthType: vi.fn(),
}))

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return { ...actual, ...api }
})

const tunnelGet = vi.hoisted(() => vi.fn())
vi.mock('@/api/tunnel', () => ({ tunnelApi: { get: tunnelGet } }))

const toast = vi.hoisted(() => ({
  success: vi.fn(),
  error: vi.fn(),
  info: vi.fn(),
  warning: vi.fn(),
}))
vi.mock('@/utils/toast', () => ({ showToast: toast }))

const { default: FlowIndex } = await import('./FlowIndex')

const ORB: WorkflowListItem = {
  id: 7,
  name: 'Opening range breakout',
  description: 'NIFTY futures at 09:20',
  is_active: true,
  created_at: '2026-10-01T09:00:00',
  updated_at: '2026-10-27T09:00:00',
  last_execution_status: 'completed',
}

const EXPIRY_DAY: WorkflowListItem = {
  id: 8,
  name: 'Expiry day straddle',
  description: null,
  is_active: false,
  created_at: '2026-10-01T09:00:00',
  updated_at: '2026-10-27T09:00:00',
  last_execution_status: null,
}

const URL_BASE = 'https://desk.ngrok-free.app/flow/webhook/wf-token'

function webhookInfo(overrides: Partial<WebhookInfo> = {}): WebhookInfo {
  return {
    webhook_token: 'wf-token',
    webhook_secret: 'wf-secret',
    webhook_enabled: true,
    webhook_auth_type: 'payload',
    webhook_url: URL_BASE,
    webhook_url_with_symbol: `${URL_BASE}/{symbol}`,
    webhook_url_with_secret: `${URL_BASE}?secret=wf-secret`,
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

function newClient() {
  return new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } },
  })
}

function EditorStub() {
  const { id } = useParams()
  return <p>editor for {id}</p>
}

function renderIndex() {
  return render(
    <MemoryRouter initialEntries={['/flow']}>
      <QueryClientProvider client={newClient()}>
        <Routes>
          <Route path="/flow" element={<FlowIndex />} />
          <Route path="/flow/editor/:id" element={<EditorStub />} />
        </Routes>
      </QueryClientProvider>
    </MemoryRouter>
  )
}

function LocationProbe() {
  return <p data-testid="location">{useLocation().pathname}</p>
}

/**
 * The list kept mounted while the address changes, so what a dialog does
 * after it navigates (close, clear, refetch the list) stays observable.
 */
function renderIndexKept() {
  return render(
    <MemoryRouter initialEntries={['/flow']}>
      <QueryClientProvider client={newClient()}>
        <FlowIndex />
        <LocationProbe />
      </QueryClientProvider>
    </MemoryRouter>
  )
}

async function openActions(name: string) {
  const user = userEvent.setup()
  const card = await screen.findByRole('link', { name: `Open workflow ${name}` })
  await user.click(within(card).getByRole('button', { name: 'Workflow actions' }))
  return user
}

async function openWebhook(name = ORB.name) {
  const user = await openActions(name)
  await user.click(await screen.findByRole('menuitem', { name: 'Webhook' }))
  const dialog = await screen.findByRole('dialog')
  return { user, dialog }
}

async function openLoadedWebhook(info: WebhookInfo = webhookInfo()) {
  api.getWebhookInfo.mockResolvedValue(info)
  const opened = await openWebhook()
  await within(opened.dialog).findByText('Enable Webhook')
  return opened
}

function iconOf(element: Element | null) {
  return element?.getAttribute('class') ?? ''
}

beforeEach(() => {
  for (const fn of Object.values(api)) fn.mockReset()
  for (const fn of Object.values(toast)) fn.mockReset()
  api.listWorkflows.mockResolvedValue([ORB, EXPIRY_DAY])
  tunnelGet.mockReset()
  tunnelGet.mockResolvedValue({ enabled: false, tunnel: { state: 'stopped', url: null } })
})

afterEach(() => {
  vi.restoreAllMocks()
})

describe('F-110 loading the list', () => {
  it('shows only a spinner until the list arrives, then the list', async () => {
    const list = deferred<WorkflowListItem[]>()
    api.listWorkflows.mockReturnValue(list.promise)
    const { container } = renderIndex()
    await waitFor(() => expect(api.listWorkflows).toHaveBeenCalledTimes(1))
    expect(container.querySelector('svg.animate-spin')).not.toBeNull()
    expect(screen.queryByRole('heading', { name: 'Flow Editor' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'New Workflow' })).not.toBeInTheDocument()
    expect(screen.queryByText('No workflows yet')).not.toBeInTheDocument()

    list.resolve([ORB])
    expect(await screen.findByText(ORB.name)).toBeInTheDocument()
    expect(container.querySelector('svg.animate-spin')).toBeNull()
  })
})

describe('F-111 the page header', () => {
  it('says what the page is for and opens each dialog from its button', async () => {
    renderIndex()
    const user = userEvent.setup()
    expect(await screen.findByRole('heading', { level: 1, name: 'Flow Editor' })).toBeVisible()
    expect(screen.getByText('Create and manage your trading automations')).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Import' }))
    expect(await screen.findByRole('dialog', { name: 'Import Workflow' })).toBeInTheDocument()
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())

    await user.click(screen.getByRole('button', { name: 'New Workflow' }))
    expect(await screen.findByRole('dialog', { name: 'Create New Workflow' })).toBeInTheDocument()
  })
})

describe('F-112 a list that could not load', () => {
  it('falls back to its own sentence when the failure carries no message, and Retry fetches again', async () => {
    api.listWorkflows.mockRejectedValueOnce('network down')
    renderIndex()
    expect(await screen.findByRole('heading', { name: 'Could not load workflows' })).toBeVisible()
    expect(
      screen.getByText('Something went wrong while loading your workflows.')
    ).toBeInTheDocument()
    expect(screen.queryByText('network down')).not.toBeInTheDocument()
    expect(screen.queryByText('No workflows yet')).not.toBeInTheDocument()
    expect(api.listWorkflows).toHaveBeenCalledTimes(1)

    await userEvent.setup().click(screen.getByRole('button', { name: 'Retry' }))
    expect(await screen.findByText(ORB.name)).toBeInTheDocument()
    expect(api.listWorkflows).toHaveBeenCalledTimes(2)
  })
})

describe('F-113 no workflows yet', () => {
  it('invites the first workflow and its button opens the create dialog', async () => {
    api.listWorkflows.mockResolvedValue([])
    renderIndex()
    expect(await screen.findByRole('heading', { name: 'No workflows yet' })).toBeVisible()
    expect(
      screen.getByText('Create your first workflow to automate your trading')
    ).toBeInTheDocument()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Create Workflow' }))
    expect(await screen.findByRole('dialog', { name: 'Create New Workflow' })).toBeInTheDocument()
  })
})

describe('F-114 the grid of cards', () => {
  it('draws one card per workflow in the order the server sent, in a 1/2/3 column grid', async () => {
    const third: WorkflowListItem = { ...EXPIRY_DAY, id: 9, name: 'Gap fill' }
    api.listWorkflows.mockResolvedValue([EXPIRY_DAY, third, ORB])
    renderIndex()
    await screen.findByText(ORB.name)
    const cards = screen.getAllByRole('link', { name: /^Open workflow / })
    expect(cards.map((card) => card.getAttribute('aria-label'))).toEqual([
      'Open workflow Expiry day straddle',
      'Open workflow Gap fill',
      'Open workflow Opening range breakout',
    ])
    const grid = cards[0].parentElement
    expect(grid).toHaveClass('grid', 'sm:grid-cols-2', 'lg:grid-cols-3')
    for (const card of cards) expect(card.parentElement).toBe(grid)
  })
})

describe('F-115 a workflow card', () => {
  it('is a focusable link that opens the editor on a mouse click', async () => {
    renderIndex()
    const card = await screen.findByRole('link', { name: `Open workflow ${ORB.name}` })
    expect(card).toHaveAttribute('tabindex', '0')
    await userEvent.setup().click(card)
    expect(await screen.findByText('editor for 7')).toBeInTheDocument()
  })

  it('opens the editor on the Space key', async () => {
    renderIndex()
    const user = userEvent.setup()
    const card = await screen.findByRole('link', { name: `Open workflow ${EXPIRY_DAY.name}` })
    card.focus()
    await user.keyboard(' ')
    expect(await screen.findByText('editor for 8')).toBeInTheDocument()
  })

  it('tints an active workflow green with a green dot, and a stopped one grey', async () => {
    renderIndex()
    const active = await screen.findByRole('link', { name: `Open workflow ${ORB.name}` })
    const stopped = screen.getByRole('link', { name: `Open workflow ${EXPIRY_DAY.name}` })
    expect(active).toHaveClass('border-green-500/30')
    expect(active.querySelector('div.h-2.w-2.rounded-full')).toHaveClass('bg-green-500')
    expect(stopped).not.toHaveClass('border-green-500/30')
    expect(stopped.querySelector('div.h-2.w-2.rounded-full')).toHaveClass('bg-muted-foreground')
  })

  it('titles the card with the name and clamps a description to two lines', async () => {
    renderIndex()
    const active = await screen.findByRole('link', { name: `Open workflow ${ORB.name}` })
    const stopped = screen.getByRole('link', { name: `Open workflow ${EXPIRY_DAY.name}` })
    expect(within(active).getByText(ORB.name)).toHaveAttribute('data-slot', 'card-title')
    expect(within(active).getByText('NIFTY futures at 09:20')).toHaveClass('line-clamp-2')
    expect(stopped.querySelector('[data-slot="card-description"]')).toBeNull()
  })
})

describe('F-116 the card footer', () => {
  const statuses: Array<[string | null, string, string[]]> = [
    ['completed', 'Last: completed', ['lucide-circle-check', 'text-green-500']],
    ['failed', 'Last: failed', ['lucide-circle-x', 'text-red-500']],
    ['running', 'Last: running', ['lucide-loader-circle', 'animate-spin']],
    ['pending', 'Last: pending', ['lucide-clock', 'text-muted-foreground']],
    ['cancelled', 'Last: cancelled', ['lucide-circle-alert']],
    [null, 'No executions', ['lucide-circle-alert']],
  ]

  it.each(statuses)('shows %s with its own icon', async (status, text, classes) => {
    api.listWorkflows.mockResolvedValue([{ ...ORB, last_execution_status: status }])
    renderIndex()
    const label = await screen.findByText(text)
    const icon = label.previousElementSibling
    expect(icon?.tagName.toLowerCase()).toBe('svg')
    for (const cls of classes) expect(iconOf(icon)).toContain(cls)
  })

  it('shows the date the workflow was last updated, not created', async () => {
    api.listWorkflows.mockResolvedValue([ORB])
    renderIndex()
    const label = await screen.findByText('Last: completed')
    const date = label.parentElement?.nextElementSibling
    expect(date?.textContent).toMatch(/\b27\b/)
    expect(date?.textContent).toMatch(/2026/)
  })
})

describe('F-117 the actions menu', () => {
  it('lists Edit, Webhook, the switch for the current state, then Delete after a separator', async () => {
    renderIndex()
    let user = await openActions(ORB.name)
    let menu = await screen.findByRole('menu')
    expect(
      within(menu)
        .getAllByRole('menuitem')
        .map((item) => item.textContent)
    ).toEqual(['Edit', 'Webhook', 'Deactivate', 'Delete'])
    expect(within(menu).getByRole('separator')).toBeInTheDocument()
    expect(within(menu).getByRole('menuitem', { name: 'Delete' })).toHaveClass('text-destructive')
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('menu')).not.toBeInTheDocument())

    user = await openActions(EXPIRY_DAY.name)
    menu = await screen.findByRole('menu')
    expect(
      within(menu)
        .getAllByRole('menuitem')
        .map((item) => item.textContent)
    ).toEqual(['Edit', 'Webhook', 'Activate', 'Delete'])
  })

  it('Edit opens the workflow in the editor', async () => {
    renderIndex()
    const user = await openActions(EXPIRY_DAY.name)
    await user.click(await screen.findByRole('menuitem', { name: 'Edit' }))
    expect(await screen.findByText('editor for 8')).toBeInTheDocument()
  })
})

describe('F-118 Activate from the list', () => {
  it('says the workflow is active and fetches the list again', async () => {
    api.activateWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    renderIndex()
    const user = await openActions(EXPIRY_DAY.name)
    expect(api.listWorkflows).toHaveBeenCalledTimes(1)
    await user.click(await screen.findByRole('menuitem', { name: 'Activate' }))
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Workflow activated', 'flow'))
    await waitFor(() => expect(api.listWorkflows).toHaveBeenCalledTimes(2))
  })

  it("shows the server's reason when it refuses", async () => {
    api.activateWorkflow.mockRejectedValue(new Error('Add a trigger before activating.'))
    renderIndex()
    const user = await openActions(EXPIRY_DAY.name)
    await user.click(await screen.findByRole('menuitem', { name: 'Activate' }))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith('Add a trigger before activating.', 'flow')
    )
    expect(toast.success).not.toHaveBeenCalled()
    expect(api.listWorkflows).toHaveBeenCalledTimes(1)
  })
})

describe('F-119 Deactivate from the list', () => {
  it('says the workflow is stopped and fetches the list again', async () => {
    api.deactivateWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    renderIndex()
    const user = await openActions(ORB.name)
    await user.click(await screen.findByRole('menuitem', { name: 'Deactivate' }))
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Workflow deactivated', 'flow'))
    await waitFor(() => expect(api.listWorkflows).toHaveBeenCalledTimes(2))
  })

  it("shows the server's reason when it refuses", async () => {
    api.deactivateWorkflow.mockRejectedValue(new Error('The workflow is busy. Try again.'))
    renderIndex()
    const user = await openActions(ORB.name)
    await user.click(await screen.findByRole('menuitem', { name: 'Deactivate' }))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith('The workflow is busy. Try again.', 'flow')
    )
    expect(api.listWorkflows).toHaveBeenCalledTimes(1)
  })
})

describe('F-120 Delete from the list', () => {
  async function confirmDelete() {
    const user = await openActions(EXPIRY_DAY.name)
    await user.click(await screen.findByRole('menuitem', { name: 'Delete' }))
    await user.click(await screen.findByRole('button', { name: 'Delete workflow' }))
  }

  it('says the workflow was deleted and fetches the list again', async () => {
    api.deleteWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    renderIndex()
    await confirmDelete()
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Workflow deleted', 'flow'))
    await waitFor(() => expect(api.listWorkflows).toHaveBeenCalledTimes(2))
  })

  it("shows the server's reason when it refuses", async () => {
    api.deleteWorkflow.mockRejectedValue(new Error('Workflow not found'))
    renderIndex()
    await confirmDelete()
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Workflow not found', 'flow'))
    expect(api.listWorkflows).toHaveBeenCalledTimes(1)
  })
})

describe('F-121 the create dialog', () => {
  async function openCreate() {
    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'New Workflow' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create New Workflow' })
    return { user, dialog }
  }

  it('asks for a name with its texts, focused on the name box', async () => {
    renderIndex()
    const { dialog } = await openCreate()
    expect(within(dialog).getByText('Give your workflow a name to get started')).toBeVisible()
    const name = within(dialog).getByLabelText('Workflow Name')
    expect(name).toHaveAttribute('placeholder', 'e.g., Morning Order Automation')
    await waitFor(() => expect(name).toHaveFocus())
  })

  it('creates on Enter, and not for a name that is only spaces', async () => {
    api.createWorkflow.mockResolvedValue({ id: 12, name: 'Gap fill' })
    renderIndex()
    const { user, dialog } = await openCreate()
    const name = within(dialog).getByLabelText('Workflow Name')
    await user.type(name, '   ')
    expect(within(dialog).getByRole('button', { name: 'Create' })).toBeDisabled()
    await user.keyboard('{Enter}')
    expect(api.createWorkflow).not.toHaveBeenCalled()

    await user.type(name, 'Gap fill{Enter}')
    await waitFor(() => expect(api.createWorkflow).toHaveBeenCalledTimes(1))
    expect(api.createWorkflow.mock.calls[0][0]).toEqual({ name: 'Gap fill' })
  })

  it('shows a spinner on Create and disables it while the request is out', async () => {
    const created = deferred<{ id: number; name: string }>()
    api.createWorkflow.mockReturnValue(created.promise)
    renderIndex()
    const { user, dialog } = await openCreate()
    await user.type(within(dialog).getByLabelText('Workflow Name'), 'Gap fill')
    const create = within(dialog).getByRole('button', { name: 'Create' })
    expect(create.querySelector('svg.animate-spin')).toBeNull()
    await user.click(create)
    await waitFor(() => expect(create).toBeDisabled())
    expect(iconOf(create.querySelector('svg'))).toContain('lucide-loader-circle')
    created.resolve({ id: 12, name: 'Gap fill' })
    expect(await screen.findByText('editor for 12')).toBeInTheDocument()
  })

  it('Cancel closes it without creating anything', async () => {
    renderIndex()
    const { user, dialog } = await openCreate()
    await user.type(within(dialog).getByLabelText('Workflow Name'), 'Gap fill')
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(api.createWorkflow).not.toHaveBeenCalled()
  })
})

describe('F-122 creating a workflow', () => {
  it('fetches the list again, closes and clears the dialog, and opens the editor without a toast', async () => {
    api.createWorkflow.mockResolvedValue({ id: 12, name: 'Gap fill' })
    renderIndexKept()
    const user = userEvent.setup()
    await screen.findByText(ORB.name)
    await user.click(screen.getByRole('button', { name: 'New Workflow' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create New Workflow' })
    await user.type(within(dialog).getByLabelText('Workflow Name'), 'Gap fill')
    await user.click(within(dialog).getByRole('button', { name: 'Create' }))

    await waitFor(() => expect(screen.getByTestId('location')).toHaveTextContent('/flow/editor/12'))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    await waitFor(() => expect(api.listWorkflows).toHaveBeenCalledTimes(2))
    expect(toast.success).not.toHaveBeenCalled()

    await user.click(screen.getByRole('button', { name: 'New Workflow' }))
    const reopened = await screen.findByRole('dialog', { name: 'Create New Workflow' })
    expect(within(reopened).getByLabelText('Workflow Name')).toHaveValue('')
  })

  it("shows the server's reason and keeps the dialog open when it refuses", async () => {
    api.createWorkflow.mockRejectedValue(new Error('Give the workflow a shorter name.'))
    renderIndexKept()
    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'New Workflow' }))
    const dialog = await screen.findByRole('dialog', { name: 'Create New Workflow' })
    await user.type(within(dialog).getByLabelText('Workflow Name'), 'Gap fill')
    await user.click(within(dialog).getByRole('button', { name: 'Create' }))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith('Give the workflow a shorter name.', 'flow')
    )
    expect(screen.getByRole('dialog', { name: 'Create New Workflow' })).toBeInTheDocument()
    expect(screen.getByTestId('location')).toHaveTextContent(/^\/flow$/)
    expect(api.listWorkflows).toHaveBeenCalledTimes(1)
  })
})

describe('F-123 IE-09 V-152 the import dialog', () => {
  async function openImport() {
    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'Import' }))
    const dialog = await screen.findByRole('dialog', { name: 'Import Workflow' })
    return { user, dialog }
  }

  it('offers a .json file or pasted JSON, with its texts', async () => {
    renderIndex()
    const { dialog } = await openImport()
    expect(
      within(dialog).getByText('Import a workflow from a JSON file or paste the JSON data')
    ).toBeVisible()
    const file = within(dialog).getByLabelText('Upload File')
    expect(file).toHaveAttribute('type', 'file')
    expect(file).toHaveAttribute('accept', '.json')
    expect(within(dialog).getByText('or paste JSON')).toBeInTheDocument()
    expect(within(dialog).getByLabelText('Workflow JSON')).toHaveAttribute(
      'placeholder',
      '{"name": "My Workflow", "nodes": [...], "edges": [...]}'
    )
  })

  it('keeps Import disabled until there is text', async () => {
    renderIndex()
    const { user, dialog } = await openImport()
    const importButton = within(dialog).getByRole('button', { name: 'Import' })
    expect(importButton).toBeDisabled()
    await user.type(within(dialog).getByLabelText('Workflow JSON'), '   ')
    expect(importButton).toBeDisabled()
    await user.type(within(dialog).getByLabelText('Workflow JSON'), '{{}')
    expect(importButton).toBeEnabled()
  })

  it('reads a chosen file into the text box', async () => {
    renderIndex()
    const { user, dialog } = await openImport()
    const text = JSON.stringify({ name: 'ORB', nodes: [], edges: [] })
    await user.upload(
      within(dialog).getByLabelText('Upload File'),
      new File([text], 'orb.json', { type: 'application/json' })
    )
    await waitFor(() => expect(within(dialog).getByLabelText('Workflow JSON')).toHaveValue(text))
  })

  it('says when the file could not be read', async () => {
    vi.spyOn(FileReader.prototype, 'readAsText').mockImplementation(function (this: FileReader) {
      this.onerror?.(new ProgressEvent('error') as ProgressEvent<FileReader>)
    })
    renderIndex()
    const { user, dialog } = await openImport()
    await user.upload(
      within(dialog).getByLabelText('Upload File'),
      new File(['{}'], 'orb.json', { type: 'application/json' })
    )
    expect(await within(dialog).findByText('Failed to read file')).toBeInTheDocument()
    expect(api.importWorkflow).not.toHaveBeenCalled()
  })

  it('closing it clears the text and the error', async () => {
    renderIndex()
    let { user, dialog } = await openImport()
    await user.click(within(dialog).getByLabelText('Workflow JSON'))
    await user.paste('{not json')
    await user.click(within(dialog).getByRole('button', { name: 'Import' }))
    expect(
      within(dialog).getByText('Invalid JSON format. Please check the workflow data.')
    ).toBeInTheDocument()
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    ;({ user, dialog } = await openImport())
    expect(within(dialog).getByLabelText('Workflow JSON')).toHaveValue('')
    expect(
      within(dialog).queryByText('Invalid JSON format. Please check the workflow data.')
    ).not.toBeInTheDocument()
  })

  it('shows a spinner on Import and disables it while the request is out', async () => {
    const imported = deferred<{ id: number; name: string }>()
    api.importWorkflow.mockReturnValue(imported.promise)
    renderIndex()
    const { user, dialog } = await openImport()
    await user.click(within(dialog).getByLabelText('Workflow JSON'))
    await user.paste(JSON.stringify({ name: 'ORB', nodes: [], edges: [] }))
    const importButton = within(dialog).getByRole('button', { name: 'Import' })
    await user.click(importButton)
    await waitFor(() => expect(importButton).toBeDisabled())
    expect(iconOf(importButton.querySelector('svg'))).toContain('lucide-loader-circle')
    imported.resolve({ id: 21, name: 'ORB' })
    expect(await screen.findByText('editor for 21')).toBeInTheDocument()
  })
})

describe('F-125 IE-09 V-152 importing a workflow', () => {
  it('names the imported workflow in a toast, fetches the list again, clears the dialog and opens the editor', async () => {
    api.importWorkflow.mockResolvedValue({ id: 21, name: 'Opening range breakout' })
    renderIndexKept()
    const user = userEvent.setup()
    await screen.findByText(ORB.name)
    await user.click(screen.getByRole('button', { name: 'Import' }))
    const dialog = await screen.findByRole('dialog', { name: 'Import Workflow' })
    await user.click(within(dialog).getByLabelText('Workflow JSON'))
    await user.paste(JSON.stringify({ name: 'Opening range breakout', nodes: [], edges: [] }))
    await user.click(within(dialog).getByRole('button', { name: 'Import' }))

    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith(
        'Workflow "Opening range breakout" imported',
        'flow'
      )
    )
    await waitFor(() => expect(screen.getByTestId('location')).toHaveTextContent('/flow/editor/21'))
    await waitFor(() => expect(api.listWorkflows).toHaveBeenCalledTimes(2))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())

    await user.click(screen.getByRole('button', { name: 'Import' }))
    const reopened = await screen.findByRole('dialog', { name: 'Import Workflow' })
    expect(within(reopened).getByLabelText('Workflow JSON')).toHaveValue('')
  })

  it("shows the server's reason in a toast and keeps the pasted text", async () => {
    api.importWorkflow.mockRejectedValue(new Error('Node node_3 has no type.'))
    renderIndexKept()
    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'Import' }))
    const dialog = await screen.findByRole('dialog', { name: 'Import Workflow' })
    const pasted = JSON.stringify({ name: 'ORB', nodes: [{}], edges: [] })
    await user.click(within(dialog).getByLabelText('Workflow JSON'))
    await user.paste(pasted)
    await user.click(within(dialog).getByRole('button', { name: 'Import' }))
    await waitFor(() =>
      expect(toast.error).toHaveBeenCalledWith('Node node_3 has no type.', 'flow')
    )
    expect(toast.success).not.toHaveBeenCalled()
    expect(within(dialog).getByLabelText('Workflow JSON')).toHaveValue(pasted)
    expect(screen.getByTestId('location')).toHaveTextContent(/^\/flow$/)
  })
})

describe('F-130 opening the webhook settings', () => {
  it('fetches the settings only once the dialog is open, with a spinner meanwhile', async () => {
    const info = deferred<WebhookInfo>()
    api.getWebhookInfo.mockReturnValue(info.promise)
    renderIndex()
    await screen.findByText(ORB.name)
    expect(api.getWebhookInfo).not.toHaveBeenCalled()

    const { dialog } = await openWebhook()
    expect(within(dialog).getByRole('heading', { name: 'Webhook Settings' })).toBeVisible()
    expect(
      within(dialog).getByText('Configure webhook for "Opening range breakout"')
    ).toBeInTheDocument()
    await waitFor(() => expect(api.getWebhookInfo).toHaveBeenCalledTimes(1))
    expect(api.getWebhookInfo.mock.calls[0][0]).toBe(7)
    expect(dialog.querySelector('svg.animate-spin')).not.toBeNull()
    expect(within(dialog).queryByText('Enable Webhook')).not.toBeInTheDocument()

    info.resolve(webhookInfo())
    expect(await within(dialog).findByText('Enable Webhook')).toBeInTheDocument()
    expect(dialog.querySelector('svg.animate-spin')).toBeNull()
  })

  it('shows nothing under the title when the settings could not be fetched', async () => {
    api.getWebhookInfo.mockRejectedValue(new Error('Workflow not found'))
    renderIndex()
    const { dialog } = await openWebhook()
    await waitFor(() => expect(dialog.querySelector('svg.animate-spin')).toBeNull())
    expect(within(dialog).getByRole('heading', { name: 'Webhook Settings' })).toBeVisible()
    expect(within(dialog).queryByText('Enable Webhook')).not.toBeInTheDocument()
    expect(within(dialog).queryByRole('switch')).not.toBeInTheDocument()
  })
})

describe('F-131 the Enable Webhook switch', () => {
  it('turns an enabled webhook off, disabled while that is out, then says so and fetches again', async () => {
    const off = deferred<{ status: string }>()
    api.disableWebhook.mockReturnValue(off.promise)
    renderIndex()
    const { user, dialog } = await openLoadedWebhook(webhookInfo({ webhook_enabled: true }))
    expect(
      within(dialog).getByText('Allow external systems to trigger this workflow')
    ).toBeVisible()
    const toggle = within(dialog).getByRole('switch')
    expect(toggle).toHaveAttribute('aria-checked', 'true')
    await user.click(toggle)
    await waitFor(() => expect(api.disableWebhook).toHaveBeenCalledWith(7))
    expect(api.enableWebhook).not.toHaveBeenCalled()
    await waitFor(() => expect(toggle).toBeDisabled())
    off.resolve({ status: 'success' })
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Webhook disabled', 'flow'))
    await waitFor(() => expect(api.getWebhookInfo).toHaveBeenCalledTimes(2))
  })

  it('turns a disabled webhook on, then says so and fetches again', async () => {
    api.enableWebhook.mockResolvedValue({ status: 'success' })
    renderIndex()
    const { user, dialog } = await openLoadedWebhook(webhookInfo({ webhook_enabled: false }))
    const toggle = within(dialog).getByRole('switch')
    expect(toggle).toHaveAttribute('aria-checked', 'false')
    await user.click(toggle)
    await waitFor(() => expect(api.enableWebhook).toHaveBeenCalledWith(7))
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Webhook enabled', 'flow'))
    await waitFor(() => expect(api.getWebhookInfo).toHaveBeenCalledTimes(2))
  })

  it("shows the server's reason when it refuses", async () => {
    api.enableWebhook.mockRejectedValue(new Error('Workflow not found'))
    renderIndex()
    const { user, dialog } = await openLoadedWebhook(webhookInfo({ webhook_enabled: false }))
    await user.click(within(dialog).getByRole('switch'))
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Workflow not found', 'flow'))
  })
})

describe('F-132 the authentication method tiles', () => {
  it('describes both tiles and highlights the one in use', async () => {
    renderIndex()
    const { dialog } = await openLoadedWebhook(webhookInfo({ webhook_auth_type: 'url' }))
    expect(within(dialog).getByText('Authentication Method')).toBeVisible()
    const payload = within(dialog).getByRole('button', { name: /Secret in Payload/ })
    const url = within(dialog).getByRole('button', { name: /Secret in URL/ })
    expect(payload).toHaveTextContent('For TradingView, custom scripts')
    expect(url).toHaveTextContent('For Chartink, fixed-format services')
    expect(url).toHaveClass('border-primary', 'bg-primary/5')
    expect(payload).not.toHaveClass('border-primary')
    expect(payload).toHaveClass('border-border')
  })

  it('switches to the secret in the payload, disabled while that is out, then says so and fetches again', async () => {
    const updated = deferred<{ status: string }>()
    api.updateWebhookAuthType.mockReturnValue(updated.promise)
    renderIndex()
    const { user, dialog } = await openLoadedWebhook(webhookInfo({ webhook_auth_type: 'url' }))
    const payload = within(dialog).getByRole('button', { name: /Secret in Payload/ })
    await user.click(payload)
    await waitFor(() => expect(api.updateWebhookAuthType).toHaveBeenCalledWith(7, 'payload'))
    await waitFor(() => expect(payload).toBeDisabled())
    expect(within(dialog).getByRole('button', { name: /Secret in URL/ })).toBeDisabled()
    updated.resolve({ status: 'success' })
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith('Authentication type updated', 'flow')
    )
    await waitFor(() => expect(api.getWebhookInfo).toHaveBeenCalledTimes(2))
  })

  it("shows the server's reason when it refuses", async () => {
    api.updateWebhookAuthType.mockRejectedValue(new Error('Invalid auth type'))
    renderIndex()
    const { user, dialog } = await openLoadedWebhook()
    await user.click(within(dialog).getByRole('button', { name: /Secret in URL/ }))
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Invalid auth type', 'flow'))
  })
})

describe('F-133 the webhook URL fields', () => {
  it('labels both URLs and says what to put in place of {symbol}', async () => {
    renderIndex()
    const { dialog } = await openLoadedWebhook()
    expect(within(dialog).getByText('Webhook URL')).toBeVisible()
    expect(within(dialog).getByText('Webhook URL (with symbol)')).toBeVisible()
    expect(within(dialog).getByDisplayValue(URL_BASE)).toHaveAttribute('readonly')
    expect(within(dialog).getByDisplayValue(`${URL_BASE}/{symbol}`)).toHaveAttribute('readonly')
    expect(
      within(dialog).getByText('Replace {symbol} with actual symbol like RELIANCE, INFY, etc.')
    ).toBeVisible()
  })

  it.each([
    ['Copy webhook URL', URL_BASE],
    ['Copy webhook URL with symbol', `${URL_BASE}/{symbol}`],
  ])('%s copies it and says so', async (button, copied) => {
    renderIndex()
    const { user, dialog } = await openLoadedWebhook()
    await user.click(within(dialog).getByRole('button', { name: button }))
    expect(await navigator.clipboard.readText()).toBe(copied)
    expect(toast.success).toHaveBeenCalledWith('URL copied to clipboard', 'clipboard')
  })
})

describe('F-134 the webhook secret', () => {
  it('hides the secret until shown, and hides it again', async () => {
    renderIndex()
    const { user, dialog } = await openLoadedWebhook()
    expect(within(dialog).getByText('Webhook Secret')).toBeVisible()
    const secret = within(dialog).getByDisplayValue('wf-secret')
    expect(secret).toHaveAttribute('type', 'password')
    await user.click(within(dialog).getByRole('button', { name: 'Show webhook secret' }))
    expect(secret).toHaveAttribute('type', 'text')
    await user.click(within(dialog).getByRole('button', { name: 'Hide webhook secret' }))
    expect(secret).toHaveAttribute('type', 'password')
    expect(within(dialog).getByRole('button', { name: 'Show webhook secret' })).toBeInTheDocument()
  })

  it('copies the secret and says so', async () => {
    renderIndex()
    const { user, dialog } = await openLoadedWebhook()
    await user.click(within(dialog).getByRole('button', { name: 'Copy webhook secret' }))
    expect(await navigator.clipboard.readText()).toBe('wf-secret')
    expect(toast.success).toHaveBeenCalledWith('Secret copied to clipboard', 'clipboard')
  })

  it.each([
    ['payload', 'Include this secret in your webhook payload as "secret" field'],
    ['url', 'Append this secret to URL as ?secret=...'],
  ] as const)('with the secret in the %s, says where it goes', async (authType, helper) => {
    renderIndex()
    const { dialog } = await openLoadedWebhook(webhookInfo({ webhook_auth_type: authType }))
    expect(within(dialog).getByText(helper)).toBeVisible()
  })
})

describe('F-135 the webhook URL with the secret', () => {
  const withSecret = `${URL_BASE}?secret=wf-secret`

  it('masks the secret in the URL until shown, and copies the full URL', async () => {
    renderIndex()
    const { user, dialog } = await openLoadedWebhook(webhookInfo({ webhook_auth_type: 'url' }))
    expect(within(dialog).getByText('Webhook URL (with secret)')).toBeVisible()
    expect(
      within(dialog).getByText('Use this URL for Chartink and other services with fixed payloads')
    ).toBeVisible()
    expect(within(dialog).getByDisplayValue(`${URL_BASE}?secret=........`)).toBeInTheDocument()
    expect(within(dialog).queryByDisplayValue(withSecret)).not.toBeInTheDocument()

    await user.click(within(dialog).getByRole('button', { name: 'Copy webhook URL with secret' }))
    expect(await navigator.clipboard.readText()).toBe(withSecret)
    expect(toast.success).toHaveBeenCalledWith('URL with secret copied to clipboard', 'clipboard')

    await user.click(within(dialog).getByRole('button', { name: 'Show webhook secret' }))
    expect(within(dialog).getByDisplayValue(withSecret)).toBeInTheDocument()
  })

  it('is not offered with the secret in the payload, or without a URL to show', async () => {
    renderIndex()
    const { dialog, user } = await openLoadedWebhook(webhookInfo({ webhook_auth_type: 'payload' }))
    expect(within(dialog).queryByText('Webhook URL (with secret)')).not.toBeInTheDocument()
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())

    api.getWebhookInfo.mockResolvedValue(
      webhookInfo({ webhook_auth_type: 'url', webhook_url_with_secret: null })
    )
    const again = await openWebhook()
    await within(again.dialog).findByText('Enable Webhook')
    expect(within(again.dialog).queryByText('Webhook URL (with secret)')).not.toBeInTheDocument()
  })
})

describe('F-136 the example usage', () => {
  const chartink = (secret: string) => `POST ${URL_BASE}?secret=${secret}

Chartink payload (sent as-is):
{
  "stocks": "RELIANCE,INFY,TCS",
  "trigger_prices": "2500.50,1800.25,3500.00",
  "triggered_at": "9:30 am",
  "scan_name": "My Scanner",
  "alert_name": "Buy Alert"
}`

  const tradingView = (secret: string) => `POST ${URL_BASE}

{
  "secret": "${secret}",
  "symbol": "RELIANCE",
  "action": "BUY",
  "quantity": 10,
  "price": 2500.50
}`

  it('shows the Chartink request with the secret in the URL, masked until shown', async () => {
    renderIndex()
    const { user, dialog } = await openLoadedWebhook(webhookInfo({ webhook_auth_type: 'url' }))
    expect(within(dialog).getByText('Example Usage')).toBeVisible()
    expect(within(dialog).getByText('For Chartink (secret in URL):')).toBeVisible()
    const example = dialog.querySelector('pre')
    expect(example?.textContent).toBe(chartink('........'))
    await user.click(within(dialog).getByRole('button', { name: 'Show webhook secret' }))
    expect(example?.textContent).toBe(chartink('wf-secret'))
  })

  it('shows the TradingView request with the secret in the payload, masked until shown', async () => {
    renderIndex()
    const { user, dialog } = await openLoadedWebhook(webhookInfo({ webhook_auth_type: 'payload' }))
    expect(within(dialog).getByText('For TradingView (secret in payload):')).toBeVisible()
    expect(within(dialog).queryByText('For Chartink (secret in URL):')).not.toBeInTheDocument()
    const example = dialog.querySelector('pre')
    expect(example?.textContent).toBe(tradingView('................'))
    await user.click(within(dialog).getByRole('button', { name: 'Show webhook secret' }))
    expect(example?.textContent).toBe(tradingView('wf-secret'))
  })
})

describe('F-137 regenerating the URL and secret', () => {
  async function confirmRegenerate() {
    const opened = await openLoadedWebhook()
    const button = within(opened.dialog).getByRole('button', { name: 'Regenerate URL & Secret' })
    await opened.user.click(button)
    await opened.user.click(await screen.findByRole('button', { name: 'Regenerate' }))
    return { ...opened, button }
  }

  it('shows a spinner and is disabled while out, then says so and fetches the new values', async () => {
    const regenerated = deferred<{ status: string }>()
    api.regenerateWebhook.mockReturnValue(regenerated.promise)
    renderIndex()
    const { button } = await confirmRegenerate()
    await waitFor(() => expect(api.regenerateWebhook).toHaveBeenCalledWith(7))
    await waitFor(() => expect(button).toBeDisabled())
    expect(iconOf(button.querySelector('svg'))).toContain('lucide-loader-circle')
    regenerated.resolve({ status: 'success' })
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith('Webhook URL and secret regenerated', 'flow')
    )
    await waitFor(() => expect(api.getWebhookInfo).toHaveBeenCalledTimes(2))
    await waitFor(() => expect(button).toBeEnabled())
    expect(iconOf(button.querySelector('svg'))).toContain('lucide-refresh-cw')
  })

  it("shows the server's reason when it refuses", async () => {
    api.regenerateWebhook.mockRejectedValue(new Error('Workflow not found'))
    renderIndex()
    await confirmRegenerate()
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Workflow not found', 'flow'))
  })
})

describe('F-139 copying does not wait for the clipboard', () => {
  it('says copied at once, while the clipboard write is still pending', async () => {
    renderIndex()
    const { user, dialog } = await openLoadedWebhook()
    const write = vi
      .spyOn(navigator.clipboard, 'writeText')
      .mockReturnValue(new Promise<void>(() => {}))
    await user.click(within(dialog).getByRole('button', { name: 'Copy webhook URL' }))
    expect(write).toHaveBeenCalledWith(URL_BASE)
    expect(toast.success).toHaveBeenCalledWith('URL copied to clipboard', 'clipboard')
  })

  it('says copied even when the clipboard refuses the write', async () => {
    renderIndex()
    const { user, dialog } = await openLoadedWebhook()
    const refused = Promise.reject(new Error('Clipboard access denied'))
    refused.catch(() => {})
    const write = vi.spyOn(navigator.clipboard, 'writeText').mockReturnValue(refused)
    await user.click(within(dialog).getByRole('button', { name: 'Copy webhook secret' }))
    expect(write).toHaveBeenCalledWith('wf-secret')
    expect(toast.success).toHaveBeenCalledWith('Secret copied to clipboard', 'clipboard')
    expect(toast.error).not.toHaveBeenCalled()
  })
})
