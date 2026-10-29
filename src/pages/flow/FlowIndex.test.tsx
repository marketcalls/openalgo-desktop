/**
 * /flow, the list of workflows: what a trader sees and what each control
 * sends. The page is the web's, carried over unchanged; the API module is
 * mocked as the web's tests mock it, and api/flow.test.ts pins the wire
 * under it.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes, useParams } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { WebhookInfo, WorkflowListItem } from '@/api/flow'
import { checkA11y } from '@/test/a11y-utils'

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

const { default: FlowIndex } = await import('./FlowIndex')

const ORB: WorkflowListItem = {
  id: 7,
  name: 'Opening range breakout',
  description: 'NIFTY futures at 09:20',
  is_active: true,
  created_at: '2026-10-29T09:00:00',
  updated_at: '2026-10-29T09:00:00',
  last_execution_status: 'completed',
}

const EXPIRY_DAY: WorkflowListItem = {
  id: 8,
  name: 'Expiry day straddle',
  description: null,
  is_active: false,
  created_at: '2026-10-29T09:00:00',
  updated_at: '2026-10-29T09:00:00',
  last_execution_status: null,
}

function webhook(url: string): WebhookInfo {
  return {
    webhook_token: 'wf-token',
    webhook_secret: 'wf-secret',
    webhook_enabled: true,
    webhook_auth_type: 'payload',
    webhook_url: url,
    webhook_url_with_symbol: `${url}/{symbol}`,
    webhook_url_with_secret: `${url}?secret=wf-secret`,
  }
}

function EditorStub() {
  const { id } = useParams()
  return <p>editor for {id}</p>
}

function renderIndex() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } },
  })
  return render(
    <MemoryRouter initialEntries={['/flow']}>
      <QueryClientProvider client={client}>
        <Routes>
          <Route path="/flow" element={<FlowIndex />} />
          <Route path="/flow/editor/:id" element={<EditorStub />} />
        </Routes>
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

beforeEach(() => {
  for (const fn of Object.values(api)) fn.mockReset()
  api.listWorkflows.mockResolvedValue([ORB, EXPIRY_DAY])
})

describe('the workflow list', () => {
  it('shows each workflow with its last run', async () => {
    const { container } = renderIndex()
    expect(await screen.findByText('Opening range breakout')).toBeInTheDocument()
    expect(screen.getByText('NIFTY futures at 09:20')).toBeInTheDocument()
    expect(screen.getByText('Last: completed')).toBeInTheDocument()
    expect(screen.getByText('No executions')).toBeInTheDocument()
    expect(screen.getByRole('heading', { level: 1, name: 'Flow Editor' })).toBeInTheDocument()
    await checkA11y(container)
  })

  it('opens a workflow from its card, by click or by keyboard', async () => {
    renderIndex()
    const user = userEvent.setup()
    const card = await screen.findByRole('link', { name: 'Open workflow Expiry day straddle' })
    card.focus()
    await user.keyboard('{Enter}')
    expect(await screen.findByText('editor for 8')).toBeInTheDocument()
  })

  it('offers to create the first workflow when there are none', async () => {
    api.listWorkflows.mockResolvedValue([])
    const { container } = renderIndex()
    expect(await screen.findByText('No workflows yet')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Create Workflow' })).toBeInTheDocument()
    await checkA11y(container)
  })

  it('says the list could not load rather than showing it empty, and retries', async () => {
    api.listWorkflows.mockRejectedValueOnce(new Error('Your session has expired. Sign in again.'))
    renderIndex()
    expect(await screen.findByText('Could not load workflows')).toBeInTheDocument()
    expect(screen.getByText('Your session has expired. Sign in again.')).toBeInTheDocument()
    expect(screen.queryByText('No workflows yet')).not.toBeInTheDocument()
    await userEvent.setup().click(screen.getByRole('button', { name: 'Retry' }))
    expect(await screen.findByText('Opening range breakout')).toBeInTheDocument()
  })
})

describe('creating and importing', () => {
  it('creates a named workflow and opens it in the editor', async () => {
    api.createWorkflow.mockResolvedValue({ id: 12, name: 'Gap fill' })
    renderIndex()
    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'New Workflow' }))
    const dialog = await screen.findByRole('dialog')
    const create = within(dialog).getByRole('button', { name: 'Create' })
    expect(create).toBeDisabled()
    await user.type(within(dialog).getByLabelText('Workflow Name'), '  Gap fill  ')
    await user.click(create)
    expect(api.createWorkflow.mock.calls[0][0]).toEqual({ name: 'Gap fill' })
    expect(await screen.findByText('editor for 12')).toBeInTheDocument()
  })

  it('refuses JSON that is not a workflow, before sending anything', async () => {
    renderIndex()
    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'Import' }))
    const dialog = await screen.findByRole('dialog')
    const json = within(dialog).getByLabelText('Workflow JSON')

    await user.click(json)
    await user.paste('{not json')
    await user.click(within(dialog).getByRole('button', { name: 'Import' }))
    expect(
      within(dialog).getByText('Invalid JSON format. Please check the workflow data.')
    ).toBeInTheDocument()

    await user.clear(json)
    await user.paste('{"name": "No graph"}')
    await user.click(within(dialog).getByRole('button', { name: 'Import' }))
    expect(
      within(dialog).getByText('Invalid workflow format. Must have name, nodes, and edges.')
    ).toBeInTheDocument()
    expect(api.importWorkflow).not.toHaveBeenCalled()
  })

  it('imports an exported workflow and opens the copy', async () => {
    api.importWorkflow.mockResolvedValue({ id: 21, name: 'Opening range breakout' })
    renderIndex()
    const user = userEvent.setup()
    await user.click(await screen.findByRole('button', { name: 'Import' }))
    const dialog = await screen.findByRole('dialog')
    const exported = { name: 'Opening range breakout', nodes: [], edges: [], version: '1.0' }
    await user.click(within(dialog).getByLabelText('Workflow JSON'))
    await user.paste(JSON.stringify(exported))
    await user.click(within(dialog).getByRole('button', { name: 'Import' }))
    expect(api.importWorkflow.mock.calls[0][0]).toEqual(exported)
    expect(await screen.findByText('editor for 21')).toBeInTheDocument()
  })
})

describe('the actions menu', () => {
  it('deactivates an active workflow and activates a stopped one', async () => {
    api.deactivateWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    api.activateWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    renderIndex()

    let user = await openActions('Opening range breakout')
    expect(screen.queryByRole('menuitem', { name: 'Activate' })).not.toBeInTheDocument()
    await user.click(await screen.findByRole('menuitem', { name: 'Deactivate' }))
    await waitFor(() => expect(api.deactivateWorkflow).toHaveBeenCalledTimes(1))
    expect(api.deactivateWorkflow.mock.calls[0][0]).toBe(7)

    user = await openActions('Expiry day straddle')
    await user.click(await screen.findByRole('menuitem', { name: 'Activate' }))
    await waitFor(() => expect(api.activateWorkflow).toHaveBeenCalledTimes(1))
    expect(api.activateWorkflow.mock.calls[0][0]).toBe(8)
  })

  it('deletes the workflow the menu belongs to', async () => {
    api.deleteWorkflow.mockResolvedValue({ status: 'success', message: 'ok' })
    renderIndex()
    const user = await openActions('Expiry day straddle')
    await user.click(await screen.findByRole('menuitem', { name: 'Delete' }))
    await waitFor(() => expect(api.deleteWorkflow).toHaveBeenCalledTimes(1))
    expect(api.deleteWorkflow.mock.calls[0][0]).toBe(8)
  })
})

describe('webhook settings (desktop)', () => {
  it('shows a URL on this computer with the note that it cannot receive alerts', async () => {
    api.getWebhookInfo.mockResolvedValue(webhook('http://127.0.0.1:5000/flow/webhook/wf-token'))
    renderIndex()
    const user = await openActions('Opening range breakout')
    await user.click(await screen.findByRole('menuitem', { name: 'Webhook' }))
    const dialog = await screen.findByRole('dialog')
    expect(
      await within(dialog).findByDisplayValue('http://127.0.0.1:5000/flow/webhook/wf-token')
    ).toBeInTheDocument()
    expect(within(dialog).getByRole('alert')).toHaveTextContent('built-in ngrok tunnel')
    expect(api.getWebhookInfo.mock.calls[0][0]).toBe(7)
  })

  it('shows a public URL without the note', async () => {
    api.getWebhookInfo.mockResolvedValue(
      webhook('https://desk.ngrok-free.app/flow/webhook/wf-token')
    )
    renderIndex()
    const user = await openActions('Opening range breakout')
    await user.click(await screen.findByRole('menuitem', { name: 'Webhook' }))
    const dialog = await screen.findByRole('dialog')
    expect(
      await within(dialog).findByDisplayValue('https://desk.ngrok-free.app/flow/webhook/wf-token')
    ).toBeInTheDocument()
    expect(within(dialog).queryByRole('alert')).not.toBeInTheDocument()
  })

  it('switches the secret into the URL for Chartink', async () => {
    api.getWebhookInfo.mockResolvedValue(
      webhook('https://desk.ngrok-free.app/flow/webhook/wf-token')
    )
    api.updateWebhookAuthType.mockResolvedValue({ status: 'success' })
    renderIndex()
    const user = await openActions('Opening range breakout')
    await user.click(await screen.findByRole('menuitem', { name: 'Webhook' }))
    const dialog = await screen.findByRole('dialog')
    await user.click(await within(dialog).findByRole('button', { name: /Secret in URL/ }))
    await waitFor(() => expect(api.updateWebhookAuthType).toHaveBeenCalledWith(7, 'url'))
  })
})
