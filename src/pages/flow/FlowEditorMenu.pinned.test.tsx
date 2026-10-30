/**
 * The Flow editor's header, toolbar and actions menu on a real canvas, pinned
 * where the audit found the behaviour carried from the web but untested: the
 * header's logo and title, the disabled-while-pending states, the menu's
 * items and their order, Export's file body and failure toasts, and the
 * Replace from JSON dialog's texts, toasts and fallbacks.
 *
 * Pins: F-164 F-165 F-166 F-167 F-168 F-169 IE-05 IE-16 V-153.
 *
 * The API module and the toasts are mocked as in FlowEditor.test.tsx.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ReplaceWorkflowResult, Workflow, WorkflowExportData } from '@/api/flow'
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

const { default: FlowEditor } = await import('./FlowEditor')

const PLACEHOLDER = '{ "name": "...", "nodes": [...], "edges": [...] }'

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
    ],
    edges: [],
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
  await user.click(screen.getByRole('button', { name: 'Workflow actions' }))
  return screen.findByRole('menu')
}

async function openReplace() {
  const user = userEvent.setup()
  await loaded()
  await openMenu(user)
  await user.click(await screen.findByRole('menuitem', { name: 'Replace from JSON' }))
  const dialog = await screen.findByRole('dialog', { name: 'Replace from JSON' })
  return { user, dialog }
}

async function pasteAndReplace(
  user: ReturnType<typeof userEvent.setup>,
  dialog: HTMLElement,
  text = '{"name": "ORB", "nodes": [], "edges": []}'
) {
  await user.click(within(dialog).getByPlaceholderText(PLACEHOLDER))
  await user.paste(text)
  await user.click(within(dialog).getByRole('button', { name: 'Replace' }))
}

function iconOf(element: Element | null) {
  return element?.getAttribute('class') ?? ''
}

/** jsdom has no Blob.text(); FileReader reads the same bytes. */
function readBlob(blob: Blob) {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve(reader.result as string)
    reader.onerror = () => reject(reader.error)
    reader.readAsText(blob)
  })
}

/** A chosen file; jsdom has no File.text(), which the Tauri webview has. */
function jsonFile(text: string) {
  const file = new File([text], 'orb.json', { type: 'application/json' })
  Object.defineProperty(file, 'text', { value: async () => text })
  return file
}

function precedes(first: Node, second: Node) {
  return Boolean(first.compareDocumentPosition(second) & Node.DOCUMENT_POSITION_FOLLOWING)
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

afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

describe('F-164 the editor header', () => {
  it('shows the logo and openalgo, Flow Editor in the middle, then Dashboard and the account controls', async () => {
    renderEditor()
    await loaded()
    const logo = screen.getByRole('img', { name: 'OpenAlgo' })
    expect(logo).toHaveAttribute('src', '/images/android-chrome-192x192.png')
    const brand = screen.getByText('openalgo')
    const title = screen.getByText('Flow Editor')
    const dashboard = screen.getByRole('link', { name: /Dashboard/ })
    const userMenu = screen.getByRole('button', { name: 'Open user menu' })
    expect(precedes(logo, brand)).toBe(true)
    expect(precedes(brand, title)).toBe(true)
    expect(precedes(title, dashboard)).toBe(true)
    expect(precedes(dashboard, userMenu)).toBe(true)
  })
})

describe('F-165 the toolbar while a request is out', () => {
  it('Save shows a spinner and is disabled while saving, and so is Activate', async () => {
    const saved = deferred<Workflow>()
    api.updateWorkflow.mockReturnValue(saved.promise)
    renderEditor()
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    const save = screen.getByRole('button', { name: /Save/ })
    expect(iconOf(save.querySelector('svg'))).toContain('lucide-save')
    await user.click(save)
    await waitFor(() => expect(save).toBeDisabled())
    // Still unsaved: Save is disabled because the save is out, not because it is clean.
    expect(screen.getByText('Unsaved')).toBeInTheDocument()
    expect(iconOf(save.querySelector('svg'))).toContain('lucide-loader-circle')
    expect(screen.getByRole('button', { name: 'Activate' })).toBeDisabled()
    saved.resolve(stored({ name: 'Opening range breakout v2' }))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Activate' })).toBeEnabled())
  })

  it('Activate is the primary button and shows a spinner, disabled, while activating', async () => {
    const activated = deferred<{ status: string; message: string }>()
    api.activateWorkflow.mockReturnValue(activated.promise)
    renderEditor()
    await loaded()
    const activate = screen.getByRole('button', { name: 'Activate' })
    expect(activate).toHaveAttribute('data-variant', 'default')
    expect(iconOf(activate.querySelector('svg'))).toContain('lucide-play')
    await userEvent.setup().click(activate)
    await waitFor(() => expect(activate).toBeDisabled())
    expect(iconOf(activate.querySelector('svg'))).toContain('lucide-loader-circle')
    activated.resolve({ status: 'success', message: 'ok' })
    expect(await screen.findByRole('button', { name: 'Deactivate' })).toBeEnabled()
  })

  it('Deactivate is an outline button and shows a spinner, disabled, while deactivating', async () => {
    const deactivated = deferred<{ status: string; message: string }>()
    api.deactivateWorkflow.mockReturnValue(deactivated.promise)
    api.getWorkflow.mockResolvedValue(stored({ is_active: true }))
    renderEditor()
    await loaded()
    const deactivate = await screen.findByRole('button', { name: 'Deactivate' })
    expect(deactivate).toHaveAttribute('data-variant', 'outline')
    expect(iconOf(deactivate.querySelector('svg'))).toContain('lucide-pause')
    await userEvent.setup().click(deactivate)
    await waitFor(() => expect(deactivate).toBeDisabled())
    expect(iconOf(deactivate.querySelector('svg'))).toContain('lucide-loader-circle')
    deactivated.resolve({ status: 'success', message: 'ok' })
    expect(await screen.findByRole('button', { name: 'Activate' })).toBeEnabled()
  })

  it('Deactivate is disabled while a save is out', async () => {
    const saved = deferred<Workflow>()
    api.updateWorkflow.mockReturnValue(saved.promise)
    api.getWorkflow.mockResolvedValue(stored({ is_active: true }))
    renderEditor()
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    await user.click(screen.getByRole('button', { name: /Save/ }))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Deactivate' })).toBeDisabled())
    saved.resolve(stored({ name: 'Opening range breakout v2', is_active: true }))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Deactivate' })).toBeEnabled())
  })
})

describe('F-166 the actions menu', () => {
  it('lists Run Now, Show Logs, Export Workflow, Replace from JSON and Keyboard Shortcuts, in that order', async () => {
    renderEditor()
    await loaded()
    const menu = await openMenu(userEvent.setup())
    expect(
      within(menu)
        .getAllByRole('menuitem')
        .map((item) => item.textContent)
    ).toEqual([
      'Run Now',
      'Show Logs',
      'Export Workflow',
      'Replace from JSON',
      'Keyboard Shortcuts',
    ])
  })

  it('Show Logs opens the execution log and Hide Logs closes it', async () => {
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    expect(screen.queryByText('Execution Log')).not.toBeInTheDocument()
    await openMenu(user)
    await user.click(await screen.findByRole('menuitem', { name: 'Show Logs' }))
    expect(await screen.findByText('Execution Log')).toBeInTheDocument()
    expect(screen.getByText('Ready')).toBeInTheDocument()
    await openMenu(user)
    await user.click(await screen.findByRole('menuitem', { name: 'Hide Logs' }))
    await waitFor(() => expect(screen.queryByText('Execution Log')).not.toBeInTheDocument())
  })

  it('Keyboard Shortcuts is a link to the shortcuts page', async () => {
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await openMenu(user)
    const link = await screen.findByRole('menuitem', { name: 'Keyboard Shortcuts' })
    expect(link.tagName).toBe('A')
    expect(link).toHaveAttribute('href', '/flow/shortcuts')
    await user.click(link)
    expect(await screen.findByText('shortcuts page')).toBeInTheDocument()
  })

  it('Run Now is disabled while a run is out', async () => {
    const ran = deferred<{ status: string; message: string }>()
    api.executeWorkflow.mockReturnValue(ran.promise)
    renderEditor()
    const user = userEvent.setup()
    await loaded()
    await openMenu(user)
    const first = await screen.findByRole('menuitem', { name: 'Run Now' })
    expect(first).not.toHaveAttribute('aria-disabled')
    await user.click(first)
    await waitFor(() => expect(api.executeWorkflow).toHaveBeenCalledTimes(1))
    await openMenu(user)
    expect(await screen.findByRole('menuitem', { name: 'Run Now' })).toHaveAttribute(
      'aria-disabled',
      'true'
    )
    ran.resolve({ status: 'success', message: 'done' })
  })

  it('Run Now is disabled while a save is out', async () => {
    const saved = deferred<Workflow>()
    api.updateWorkflow.mockReturnValue(saved.promise)
    renderEditor()
    const user = userEvent.setup()
    await user.type(await loaded(), ' v2')
    await user.click(screen.getByRole('button', { name: /Save/ }))
    await waitFor(() => expect(api.updateWorkflow).toHaveBeenCalledTimes(1))
    await openMenu(user)
    expect(await screen.findByRole('menuitem', { name: 'Run Now' })).toHaveAttribute(
      'aria-disabled',
      'true'
    )
    saved.resolve(stored({ name: 'Opening range breakout v2' }))
  })
})

describe('F-167 IE-05 Export Workflow', () => {
  const exported: WorkflowExportData = {
    version: '1.0',
    name: 'Opening Range #2',
    description: null,
    nodes: [{ id: 'node_1', type: 'start', position: { x: 0, y: 0 }, data: { time: '09:20' } }],
    edges: [],
    exported_at: '2026-10-30T09:00:00',
  }

  function captureDownload() {
    const createObjectURL = vi.fn((_blob: Blob) => 'blob:flow-export')
    vi.stubGlobal('URL', Object.assign(URL, { createObjectURL, revokeObjectURL: vi.fn() }))
    const clicked: HTMLAnchorElement[] = []
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(function (
      this: HTMLAnchorElement
    ) {
      clicked.push(this)
    })
    return { createObjectURL, clicked }
  }

  async function exportNow() {
    const user = userEvent.setup()
    await loaded()
    await openMenu(user)
    await user.click(await screen.findByRole('menuitem', { name: 'Export Workflow' }))
  }

  it('writes the stored workflow as JSON indented by two spaces', async () => {
    api.exportWorkflow.mockResolvedValue(exported)
    const { createObjectURL, clicked } = captureDownload()
    renderEditor()
    await exportNow()
    await waitFor(() => expect(clicked).toHaveLength(1))
    expect(api.exportWorkflow).toHaveBeenCalledWith(7)
    const blob = createObjectURL.mock.calls[0][0]
    expect(await readBlob(blob)).toBe(JSON.stringify(exported, null, 2))
    expect(toast.success).toHaveBeenCalledWith('Workflow exported', 'flow')
  })

  it('shows the reason when the export fails, and downloads nothing', async () => {
    api.exportWorkflow.mockRejectedValue(new Error('Workflow not found'))
    const { clicked } = captureDownload()
    renderEditor()
    await exportNow()
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Workflow not found', 'flow'))
    expect(clicked).toHaveLength(0)
    expect(toast.success).not.toHaveBeenCalled()
  })

  it('says Export failed when the failure carries no message', async () => {
    api.exportWorkflow.mockRejectedValue('offline')
    captureDownload()
    renderEditor()
    await exportNow()
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith('Export failed', 'flow'))
  })
})

describe('F-168 IE-16 the Replace from JSON dialog', () => {
  it('explains what is replaced and what is kept, and how the JSON is checked', async () => {
    renderEditor()
    const { dialog } = await openReplace()
    expect(
      within(dialog).getByText(
        "Replaces this workflow's nodes and edges in place. The workflow id, webhook URL and active state are kept, so nothing pointing at this workflow breaks. Export first if you want a copy of the current version."
      )
    ).toBeVisible()
    expect(
      within(dialog).getByText(
        'Validated the same way as Import: a graph that could not run is rejected with the reason, rather than saved to fail later.'
      )
    ).toBeVisible()
    const file = dialog.querySelector('input[type="file"]')
    expect(file).toHaveAttribute('accept', 'application/json,.json')
    expect(within(dialog).getByPlaceholderText(PLACEHOLDER)).toHaveValue('')
  })

  it('reads a chosen file into the text box', async () => {
    renderEditor()
    const { user, dialog } = await openReplace()
    const text = '{"name": "ORB", "nodes": [], "edges": []}'
    const file = dialog.querySelector('input[type="file"]') as HTMLInputElement
    await user.upload(file, jsonFile(text))
    await waitFor(() => expect(within(dialog).getByPlaceholderText(PLACEHOLDER)).toHaveValue(text))
  })

  it('keeps Replace disabled until there is text, and Cancel closes without replacing', async () => {
    renderEditor()
    const { user, dialog } = await openReplace()
    const replace = within(dialog).getByRole('button', { name: 'Replace' })
    expect(replace).toBeDisabled()
    await user.type(within(dialog).getByPlaceholderText(PLACEHOLDER), '   ')
    expect(replace).toBeDisabled()
    await user.type(within(dialog).getByPlaceholderText(PLACEHOLDER), '{{}')
    expect(replace).toBeEnabled()
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(api.replaceWorkflow).not.toHaveBeenCalled()
  })

  it('says Replacing... and is disabled while the replacement is out', async () => {
    const replaced = deferred<ReplaceWorkflowResult>()
    api.replaceWorkflow.mockReturnValue(replaced.promise)
    renderEditor()
    const { user, dialog } = await openReplace()
    await pasteAndReplace(user, dialog)
    const busy = await within(dialog).findByRole('button', { name: 'Replacing...' })
    expect(busy).toBeDisabled()
    replaced.resolve({ status: 'success', workflow_id: 7 })
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
  })
})

describe('F-169 IE-16 V-153 replacing the workflow', () => {
  it('says Workflow replaced. and closes and clears the dialog', async () => {
    api.replaceWorkflow.mockResolvedValue({ status: 'success', workflow_id: 7 })
    renderEditor()
    const { user, dialog } = await openReplace()
    await pasteAndReplace(user, dialog)
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Workflow replaced.', 'flow'))
    expect(api.replaceWorkflow).toHaveBeenCalledWith(7, { name: 'ORB', nodes: [], edges: [] })
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(toast.warning).not.toHaveBeenCalled()

    await openMenu(user)
    await user.click(await screen.findByRole('menuitem', { name: 'Replace from JSON' }))
    const reopened = await screen.findByRole('dialog', { name: 'Replace from JSON' })
    expect(within(reopened).getByPlaceholderText(PLACEHOLDER)).toHaveValue('')
  })

  it('counts the legacy fields the server upgraded', async () => {
    api.replaceWorkflow.mockResolvedValue({
      status: 'success',
      workflow_id: 7,
      migrations: ['node_2: condition >= kept as >=', 'node_3: interval 1d read as D'],
    })
    renderEditor()
    const { user, dialog } = await openReplace()
    await pasteAndReplace(user, dialog)
    await waitFor(() =>
      expect(toast.success).toHaveBeenCalledWith(
        'Workflow replaced. 2 legacy field(s) upgraded.',
        'flow'
      )
    )
  })

  it('puts the count before the deactivation warning', async () => {
    api.replaceWorkflow.mockResolvedValue({
      status: 'success',
      workflow_id: 7,
      needs_reactivate: true,
      migrations: ['node_2: interval 1d read as D'],
    })
    renderEditor()
    const { user, dialog } = await openReplace()
    await pasteAndReplace(user, dialog)
    await waitFor(() =>
      expect(toast.warning).toHaveBeenCalledWith(
        'Workflow replaced. 1 legacy field(s) upgraded. The trigger changed, so the workflow was deactivated. Activate it again once the trigger is valid.',
        'flow'
      )
    )
    expect(toast.success).not.toHaveBeenCalled()
  })

  it('shows the error message under the text when the reply carries no reason', async () => {
    api.replaceWorkflow.mockRejectedValue(new Error('Network Error'))
    renderEditor()
    const { user, dialog } = await openReplace()
    await pasteAndReplace(user, dialog)
    expect(await within(dialog).findByText('Network Error')).toBeInTheDocument()
    expect(within(dialog).getByRole('button', { name: 'Replace' })).toBeEnabled()
    expect(toast.success).not.toHaveBeenCalled()
  })

  it('says Replace failed when the failure carries no message at all', async () => {
    api.replaceWorkflow.mockRejectedValue('offline')
    renderEditor()
    const { user, dialog } = await openReplace()
    await pasteAndReplace(user, dialog)
    expect(await within(dialog).findByText('Replace failed')).toBeInTheDocument()
  })
})
