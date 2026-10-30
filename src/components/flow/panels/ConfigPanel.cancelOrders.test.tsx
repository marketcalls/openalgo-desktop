/**
 * The cancel nodes' panels say what the web's say (ConfigPanel.tsx, equal to
 * the web's): Cancel Order points the trader at the Place Order result
 * (parity N-cancelOrder-01), Cancel All Orders says it needs nothing
 * (N-cancelAllOrders-01).
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { DEFAULT_NODE_DATA } from '@/lib/flow/constants'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return {
    ...actual,
    getIndexSymbolsLotSizes: vi.fn(async () => []),
    getSymbolLotSizes: vi.fn(async () => ({})),
  }
})

vi.mock('@/api/tunnel', () => ({
  tunnelApi: { get: vi.fn(async () => ({ tunnel: { state: 'stopped', url: null } })) },
}))

const { ConfigPanel } = await import('./ConfigPanel')

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

describe('the cancel order panels', () => {
  it('Cancel Order takes the order id from the Place Order result', () => {
    openPanelFor('cancelOrder')
    const field = screen.getByPlaceholderText('{{orderResult.orderid}}')
    expect(field).toHaveValue('')
    expect(screen.getByText('Use variable from Place Order')).toBeInTheDocument()
  })

  it('Cancel All Orders says there is nothing to configure', () => {
    openPanelFor('cancelAllOrders')
    expect(
      screen.getByText('Cancels all open orders. No configuration needed.')
    ).toBeInTheDocument()
    expect(screen.queryByRole('textbox')).toBeNull()
  })
})
