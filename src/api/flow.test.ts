/**
 * The Flow editor's side of the /flow/api contract.
 *
 * The pages are carried over from the web unchanged, so they send exactly
 * what the web's pages send. This pins that wire, request by request: the
 * method, the path under /flow/api, the body, the CSRF header on every write,
 * and which part of the reply each function hands back. The desktop's Rust
 * routes (blueprints/flow.py's) must answer these, and a change here is a
 * change the server lanes must make too.
 *
 * Requests go through the real webClient (its CSRF and error interceptors)
 * with an adapter that records them instead of sending them.
 */

import type { AxiosAdapter, AxiosResponse, InternalAxiosRequestConfig } from 'axios'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { webClient } from './client'
import {
  activateWorkflow,
  createWorkflow,
  deactivateWorkflow,
  deleteWorkflow,
  disableWebhook,
  enableWebhook,
  executeWorkflow,
  exportWorkflow,
  flowQueryKeys,
  getIndexSymbolsLotSizes,
  getOptionStrikes,
  getSymbolLotSizes,
  getWebhookInfo,
  getWorkflow,
  getWorkflowExecutions,
  importWorkflow,
  listWorkflows,
  regenerateWebhook,
  regenerateWebhookSecret,
  replaceWorkflow,
  updateWebhookAuthType,
  updateWorkflow,
  type WorkflowExportData,
} from './flow'

const CSRF = 'csrf-for-tests'

interface Sent {
  method: string
  url: string
  params: unknown
  body: unknown
  csrf: unknown
}

let sent: Sent[] = []
let reply: unknown = {}
const originalAdapter = webClient.defaults.adapter

const recorder: AxiosAdapter = async (config: InternalAxiosRequestConfig) => {
  sent.push({
    method: (config.method ?? '').toUpperCase(),
    url: config.url ?? '',
    params: config.params,
    body: typeof config.data === 'string' ? JSON.parse(config.data) : config.data,
    csrf: config.headers?.['X-CSRFToken'],
  })
  const response: AxiosResponse = {
    data: reply,
    status: 200,
    statusText: 'OK',
    headers: {},
    config,
    request: {},
  }
  return response
}

beforeEach(() => {
  sent = []
  reply = {}
  webClient.defaults.adapter = recorder
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({ json: async () => ({ csrf_token: CSRF }) }) as unknown as Response)
  )
})

afterEach(() => {
  webClient.defaults.adapter = originalAdapter
  vi.unstubAllGlobals()
})

const EXPORTED: WorkflowExportData = {
  version: '1.0',
  name: 'Opening range',
  description: null,
  nodes: [{ id: 'n1', type: 'start', position: { x: 0, y: 0 }, data: {} }],
  edges: [],
  exported_at: '2026-10-30T09:15:00',
}

describe('the requests the editor sends', () => {
  it.each([
    ['list workflows', () => listWorkflows(), 'GET', '/flow/api/workflows'],
    ['open a workflow', () => getWorkflow(7), 'GET', '/flow/api/workflows/7'],
    ['delete a workflow', () => deleteWorkflow(7), 'DELETE', '/flow/api/workflows/7'],
    ['activate', () => activateWorkflow(7), 'POST', '/flow/api/workflows/7/activate'],
    ['deactivate', () => deactivateWorkflow(7), 'POST', '/flow/api/workflows/7/deactivate'],
    ['run now', () => executeWorkflow(7), 'POST', '/flow/api/workflows/7/execute'],
    [
      'execution history (default 20)',
      () => getWorkflowExecutions(7),
      'GET',
      '/flow/api/workflows/7/executions?limit=20',
    ],
    [
      'execution history (a limit)',
      () => getWorkflowExecutions(7, 50),
      'GET',
      '/flow/api/workflows/7/executions?limit=50',
    ],
    ['webhook settings', () => getWebhookInfo(7), 'GET', '/flow/api/workflows/7/webhook'],
    ['enable the webhook', () => enableWebhook(7), 'POST', '/flow/api/workflows/7/webhook/enable'],
    [
      'disable the webhook',
      () => disableWebhook(7),
      'POST',
      '/flow/api/workflows/7/webhook/disable',
    ],
    [
      'new webhook URL and secret',
      () => regenerateWebhook(7),
      'POST',
      '/flow/api/workflows/7/webhook/regenerate',
    ],
    [
      'new webhook secret only',
      () => regenerateWebhookSecret(7),
      'POST',
      '/flow/api/workflows/7/webhook/regenerate-secret',
    ],
    ['export', () => exportWorkflow(7), 'GET', '/flow/api/workflows/7/export'],
    ['index lot sizes', () => getIndexSymbolsLotSizes(), 'GET', '/flow/api/index-symbols'],
  ])('%s', async (_label, call, method, url) => {
    await call()
    expect(sent).toHaveLength(1)
    expect(sent[0]).toMatchObject({ method, url })
    // Flask-WTF protects every write; the desktop's server checks the same token.
    expect(sent[0].csrf).toBe(method === 'GET' ? undefined : CSRF)
  })

  it('creates a workflow with its name, description and graph', async () => {
    await createWorkflow({ name: 'Opening range', description: 'NIFTY', nodes: [], edges: [] })
    expect(sent[0]).toMatchObject({
      method: 'POST',
      url: '/flow/api/workflows',
      body: { name: 'Opening range', description: 'NIFTY', nodes: [], edges: [] },
      csrf: CSRF,
    })
  })

  it('saves with PUT and the edited fields only', async () => {
    reply = { id: 7, needs_reactivate: true }
    const saved = await updateWorkflow(7, { name: 'Renamed' })
    expect(sent[0]).toMatchObject({
      method: 'PUT',
      url: '/flow/api/workflows/7',
      body: { name: 'Renamed' },
      csrf: CSRF,
    })
    expect(saved.needs_reactivate).toBe(true)
  })

  it('switches the webhook auth type with an auth_type field', async () => {
    await updateWebhookAuthType(7, 'url')
    expect(sent[0]).toMatchObject({
      method: 'POST',
      url: '/flow/api/workflows/7/webhook/auth-type',
      body: { auth_type: 'url' },
      csrf: CSRF,
    })
  })

  it('imports the exported document as sent', async () => {
    await importWorkflow(EXPORTED)
    expect(sent[0]).toMatchObject({
      method: 'POST',
      url: '/flow/api/workflows/import',
      body: EXPORTED,
      csrf: CSRF,
    })
  })

  it('replaces a graph in place under the same id', async () => {
    await replaceWorkflow(7, EXPORTED)
    expect(sent[0]).toMatchObject({
      method: 'POST',
      url: '/flow/api/workflows/7/replace',
      body: EXPORTED,
      csrf: CSRF,
    })
  })

  it('asks for option strikes with query parameters', async () => {
    reply = { status: 'success', data: { underlying: 'NIFTY', strikes: [] } }
    await getOptionStrikes({ underlying: 'NIFTY', expiryType: 'current_week', optionType: 'CE' })
    expect(sent[0]).toMatchObject({
      method: 'GET',
      url: '/flow/api/option-strikes',
      params: { underlying: 'NIFTY', expiryType: 'current_week', optionType: 'CE' },
    })
  })

  it('looks lot sizes up in one batch', async () => {
    reply = { status: 'success', lotSizes: { 'NFO:NIFTY28OCT25FUT': 75 } }
    const refs = [
      { symbol: 'NIFTY28OCT25FUT', exchange: 'NFO' },
      { symbol: 'SBIN', exchange: 'NSE' },
    ]
    await getSymbolLotSizes(refs)
    expect(sent).toHaveLength(1)
    expect(sent[0]).toMatchObject({
      method: 'POST',
      url: '/flow/api/symbol-lotsizes',
      body: { symbols: refs },
      csrf: CSRF,
    })
  })
})

describe('what each function hands back', () => {
  it('an import answers with the new id and the imported name', async () => {
    reply = { status: 'success', workflow_id: 42 }
    expect(await importWorkflow(EXPORTED)).toEqual({ id: 42, name: 'Opening range' })
    expect(await importWorkflow({ ...EXPORTED, name: '' })).toEqual({
      id: 42,
      name: 'Imported Workflow',
    })
  })

  it('index lot sizes unwrap data, and an empty reply is an empty list', async () => {
    reply = { status: 'success', data: [{ value: 'NIFTY', label: 'NIFTY', lotSize: 75 }] }
    expect(await getIndexSymbolsLotSizes()).toEqual([
      { value: 'NIFTY', label: 'NIFTY', lotSize: 75 },
    ])
    reply = { status: 'success' }
    expect(await getIndexSymbolsLotSizes()).toEqual([])
  })

  it('option strikes unwrap data', async () => {
    reply = { status: 'success', data: { underlying: 'GOLDM', exchange: 'MCX', atm: 163000 } }
    expect(await getOptionStrikes({ underlying: 'GOLDM' })).toEqual({
      underlying: 'GOLDM',
      exchange: 'MCX',
      atm: 163000,
    })
  })

  it('a lot size that is not a positive whole number reads as none', async () => {
    reply = {
      status: 'success',
      lotSizes: {
        'NFO:A': 75,
        'NFO:B': 0,
        'NFO:C': -5,
        'NFO:D': 1.5,
        'NFO:E': null,
        'NFO:F': '75',
      },
    }
    expect(await getSymbolLotSizes([{ symbol: 'A', exchange: 'NFO' }])).toEqual({
      'NFO:A': 75,
      'NFO:B': null,
      'NFO:C': null,
      'NFO:D': null,
      'NFO:E': null,
      'NFO:F': null,
    })
  })

  it('an empty basket asks nothing, and a reply without lot sizes is empty', async () => {
    expect(await getSymbolLotSizes([])).toEqual({})
    expect(sent).toHaveLength(0)
    reply = { status: 'success' }
    expect(await getSymbolLotSizes([{ symbol: 'A', exchange: 'NFO' }])).toEqual({})
  })

  it('the other calls hand back the reply body unchanged', async () => {
    reply = [{ id: 1, name: 'A', is_active: false }]
    expect(await listWorkflows()).toEqual(reply)
    reply = { status: 'success', message: 'Workflow activated', job_id: 'wf-1' }
    expect(await activateWorkflow(1)).toEqual(reply)
  })
})

describe('query keys', () => {
  it('nest every workflow key under the workflow, so one invalidation covers it', () => {
    expect(flowQueryKeys.workflow(7)).toEqual(['flow', 'workflows', 7])
    expect(flowQueryKeys.executions(7)).toEqual(['flow', 'workflows', 7, 'executions'])
    expect(flowQueryKeys.webhook(7)).toEqual(['flow', 'workflows', 7, 'webhook'])
    expect(flowQueryKeys.indexSymbols()).toEqual(['flow', 'index-symbols'])
    expect(flowQueryKeys.optionStrikes('NIFTY', '28OCT25', 'CE')).toEqual([
      'flow',
      'option-strikes',
      'NIFTY',
      '28OCT25',
      'CE',
    ])
    expect(flowQueryKeys.symbolLotSizes(['NFO:A'])).toEqual(['flow', 'symbol-lotsizes', ['NFO:A']])
  })
})
