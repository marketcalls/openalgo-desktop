/**
 * /flow, the workflow list: the details the wave 6 audit found still unpinned
 * after FlowIndex.pinned.test.tsx. Each condition of the import check alone,
 * the header buttons' variants, where the menu separator sits, Edit opening
 * the editor once, the webhook switch while Enable is out and a refused
 * Disable, and the payload tile marked as the method in use.
 *
 * Pins: V-152 IE-09 F-124 F-111 F-117 F-131 F-132.
 *
 * The harness is FlowIndex.pinned.test.tsx's: the API module and the toasts
 * mocked, a refetch observed as one more call of the mocked request.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, useLocation, useNavigate } from 'react-router'
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

const NOT_A_WORKFLOW = 'Invalid workflow format. Must have name, nodes, and edges.'

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

function LocationProbe() {
  return <p data-testid="location">{useLocation().pathname}</p>
}

/** The browser's Back, to count how many entries a click pushed. */
function BackProbe() {
  const navigate = useNavigate()
  return (
    <button type="button" onClick={() => navigate(-1)}>
      history back
    </button>
  )
}

/** The list kept mounted while the address changes, so history stays observable. */
function renderIndex() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } },
  })
  return render(
    <MemoryRouter initialEntries={['/flow']}>
      <QueryClientProvider client={client}>
        <FlowIndex />
        <LocationProbe />
        <BackProbe />
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

async function openLoadedWebhook(info: WebhookInfo) {
  api.getWebhookInfo.mockResolvedValue(info)
  const user = await openActions(ORB.name)
  await user.click(await screen.findByRole('menuitem', { name: 'Webhook' }))
  const dialog = await screen.findByRole('dialog')
  await within(dialog).findByText('Enable Webhook')
  return { user, dialog }
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

describe('V-152 IE-09 F-124 each condition of the import check alone', () => {
  it.each([
    ['no name, with both arrays', { nodes: [], edges: [] }],
    ['an empty name, with both arrays', { name: '', nodes: [], edges: [] }],
    ['nodes that are not an array', { name: 'ORB', nodes: { node_1: {} }, edges: [] }],
    ['no nodes at all', { name: 'ORB', edges: [] }],
    ['edges that are not an array', { name: 'ORB', nodes: [], edges: 'node_1->node_2' }],
    ['no edges at all', { name: 'ORB', nodes: [] }],
  ])('refuses %s with the same sentence and sends nothing', async (_case, payload) => {
    renderIndex()
    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'Import' }))
    const dialog = await screen.findByRole('dialog', { name: 'Import Workflow' })
    await user.click(within(dialog).getByLabelText('Workflow JSON'))
    await user.paste(JSON.stringify(payload))
    await user.click(within(dialog).getByRole('button', { name: 'Import' }))
    expect(await within(dialog).findByText(NOT_A_WORKFLOW)).toBeInTheDocument()
    expect(api.importWorkflow).not.toHaveBeenCalled()
    expect(screen.getByTestId('location')).toHaveTextContent(/^\/flow$/)
  })

  it('sends a workflow with a name and both arrays as it was pasted', async () => {
    api.importWorkflow.mockResolvedValue({ id: 21, name: 'ORB' })
    renderIndex()
    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'Import' }))
    const dialog = await screen.findByRole('dialog', { name: 'Import Workflow' })
    const payload = { name: 'ORB', nodes: [{ id: 'node_1', type: 'start' }], edges: [] }
    await user.click(within(dialog).getByLabelText('Workflow JSON'))
    await user.paste(JSON.stringify(payload))
    await user.click(within(dialog).getByRole('button', { name: 'Import' }))
    await waitFor(() => expect(api.importWorkflow).toHaveBeenCalledTimes(1))
    expect(api.importWorkflow.mock.calls[0][0]).toEqual(payload)
    expect(screen.queryByText(NOT_A_WORKFLOW)).not.toBeInTheDocument()
  })
})

describe('F-111 the header buttons', () => {
  it('draws Import as an outline button beside the primary New Workflow', async () => {
    renderIndex()
    const importButton = await screen.findByRole('button', { name: 'Import' })
    expect(importButton).toHaveAttribute('data-variant', 'outline')
    expect(screen.getByRole('button', { name: 'New Workflow' })).toHaveAttribute(
      'data-variant',
      'default'
    )
  })
})

describe('F-117 the card actions menu', () => {
  it('puts the separator right before Delete, after the activate switch', async () => {
    renderIndex()
    await openActions(ORB.name)
    const menu = await screen.findByRole('menu')
    const separator = within(menu).getByRole('separator')
    expect(separator.previousElementSibling).toBe(
      within(menu).getByRole('menuitem', { name: 'Deactivate' })
    )
    expect(separator.nextElementSibling).toBe(
      within(menu).getByRole('menuitem', { name: 'Delete' })
    )
  })

  it('Edit opens the editor once: the click does not also open it through the card', async () => {
    renderIndex()
    const user = await openActions(EXPIRY_DAY.name)
    await user.click(await screen.findByRole('menuitem', { name: 'Edit' }))
    await waitFor(() => expect(screen.getByTestId('location')).toHaveTextContent('/flow/editor/8'))
    // One entry pushed: Back returns to the list, not to a second copy of the editor.
    await user.click(screen.getByRole('button', { name: 'history back' }))
    await waitFor(() => expect(screen.getByTestId('location')).toHaveTextContent(/^\/flow$/))
  })
})

describe('F-131 the Enable Webhook switch', () => {
  it('is disabled while turning the webhook on is out, and usable again after', async () => {
    const on = deferred<{ status: string }>()
    api.enableWebhook.mockReturnValue(on.promise)
    renderIndex()
    const { user, dialog } = await openLoadedWebhook(webhookInfo({ webhook_enabled: false }))
    const toggle = within(dialog).getByRole('switch')
    expect(toggle).toBeEnabled()
    await user.click(toggle)
    await waitFor(() => expect(api.enableWebhook).toHaveBeenCalledWith(7))
    await waitFor(() => expect(toggle).toBeDisabled())
    api.getWebhookInfo.mockResolvedValue(webhookInfo({ webhook_enabled: true }))
    on.resolve({ status: 'success' })
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Webhook enabled', 'flow'))
    await waitFor(() => expect(within(dialog).getByRole('switch')).toBeEnabled())
    expect(within(dialog).getByRole('switch')).toHaveAttribute('aria-checked', 'true')
  })

  it("shows the server's reason when turning the webhook off is refused", async () => {
    api.disableWebhook.mockRejectedValue(new Error('Workflow not found'))
    renderIndex()
    const { user, dialog } = await openLoadedWebhook(webhookInfo({ webhook_enabled: true }))
    await user.click(within(dialog).getByRole('switch'))
    await waitFor(() => expect(api.disableWebhook).toHaveBeenCalledWith(7))
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Workflow not found', 'flow'))
    expect(toast.success).not.toHaveBeenCalled()
    expect(api.getWebhookInfo).toHaveBeenCalledTimes(1)
  })
})

describe('F-132 the authentication method tiles', () => {
  it('marks Secret in Payload as the method in use when the secret goes in the payload', async () => {
    renderIndex()
    const { dialog } = await openLoadedWebhook(webhookInfo({ webhook_auth_type: 'payload' }))
    const payload = within(dialog).getByRole('button', { name: /Secret in Payload/ })
    const url = within(dialog).getByRole('button', { name: /Secret in URL/ })
    expect(payload).toHaveClass('border-primary', 'bg-primary/5')
    expect(payload).not.toHaveClass('border-border')
    expect(url).toHaveClass('border-border')
    expect(url).not.toHaveClass('border-primary')
    expect(url).not.toHaveClass('bg-primary/5')
  })
})
