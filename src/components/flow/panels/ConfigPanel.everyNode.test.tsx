/**
 * The config panel opens for every node type a trader can drop, from that
 * type's defaults, under the title the canvas shows. ConfigPanel is the web's
 * 3,449-line switch over node types; a type that falls through it, or a
 * default its fields cannot read, is a node that cannot be configured.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen, waitFor, within } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { DEFAULT_NODE_DATA, NODE_DEFINITIONS } from '@/lib/flow/constants'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

const LOCAL_URL = 'http://127.0.0.1:5000/flow/webhook/wf-token'

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return {
    ...actual,
    getIndexSymbolsLotSizes: vi.fn(async () => [
      { value: 'NIFTY', label: 'NIFTY', exchange: 'NSE_INDEX', lotSize: 75 },
    ]),
    getWebhookInfo: vi.fn(async () => ({
      webhook_token: 'wf-token',
      webhook_secret: 'wf-secret',
      webhook_enabled: true,
      webhook_auth_type: 'payload',
      webhook_url: LOCAL_URL,
      webhook_url_with_symbol: `${LOCAL_URL}/{symbol}`,
      webhook_url_with_secret: null,
    })),
    getSymbolLotSizes: vi.fn(async () => ({})),
    getOptionStrikes: vi.fn(async () => ({
      underlying: 'NIFTY',
      exchange: 'NFO',
      expiry: '28OCT25',
      expiries: ['28OCT25'],
      resolved: { current_week: '28OCT25' },
      optionType: 'CE',
      strikes: [{ strike: 25000, symbol: 'NIFTY28OCT2525000CE', label: 'ATM' }],
      atm: 25000,
      underlyingLtp: 25010,
      underlyingSymbol: 'NIFTY',
    })),
  }
})

const { ConfigPanel } = await import('./ConfigPanel')

const definitions = Object.values(NODE_DEFINITIONS).flat()

function openPanelFor(type: string) {
  const defaults = (DEFAULT_NODE_DATA as Record<string, Record<string, unknown>>)[type]
  useFlowWorkflowStore.setState({
    nodes: [{ id: `n-${type}`, type, position: { x: 0, y: 0 }, data: { ...defaults } }],
    selectedNodeId: `n-${type}`,
  })
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })
  return render(
    <MemoryRouter initialEntries={['/flow/editor/7']}>
      <QueryClientProvider client={client}>
        <Routes>
          <Route path="/flow/editor/:id" element={<ConfigPanel />} />
        </Routes>
      </QueryClientProvider>
    </MemoryRouter>
  )
}

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('the config panel for every node type', () => {
  it.each(definitions.map((d) => [d.type, d.label]))('%s opens from its defaults', (type) => {
    openPanelFor(type)
    const title = screen.getByRole('heading', { level: 2 })
    expect(title.textContent?.trim()).not.toBe('')
    // Below the title every type has a body: its fields, or for the two
    // types with nothing to set (cancel all orders, NOT gate), a sentence
    // saying what the node does.
    const body = document.querySelector('.space-y-4.p-4')
    expect(body?.childElementCount, type).toBeGreaterThan(0)
    expect(body?.textContent?.trim(), type).not.toBe('')
  })

  it('shows nothing when no node is selected', () => {
    useFlowWorkflowStore.setState({ nodes: [], selectedNodeId: null })
    const client = new QueryClient()
    const { container } = render(
      <MemoryRouter>
        <QueryClientProvider client={client}>
          <ConfigPanel />
        </QueryClientProvider>
      </MemoryRouter>
    )
    expect(container.querySelector('h2')).toBeNull()
  })
})

describe('the webhook trigger panel on the desktop', () => {
  it('shows the URL and says a URL on this computer cannot receive alerts', async () => {
    openPanelFor('webhookTrigger')
    await waitFor(() => expect(screen.getByDisplayValue(LOCAL_URL)).toBeInTheDocument())
    const alert = screen.getByRole('alert')
    expect(within(alert).getByText('Webhook URL not accessible!')).toBeInTheDocument()
  })
})
