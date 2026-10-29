/**
 * The config panel's departures from the web, each a web defect named in
 * .lane/105/parity.md: what the panel shows must be what the run uses, and it
 * must not promise what the run does not do.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }))
vi.mock('@/utils/toast', () => ({
  showToast: { success: toast.success, error: toast.error, info: vi.fn(), warning: vi.fn() },
}))

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
  getWebhookInfo: vi.fn(async () => ({
    webhook_token: 't',
    webhook_secret: 's',
    webhook_enabled: true,
    webhook_auth_type: 'payload',
    webhook_url: 'https://desk.ngrok-free.app/flow/webhook/t',
    webhook_url_with_symbol: 'https://desk.ngrok-free.app/flow/webhook/t/{symbol}',
    webhook_url_with_secret: null,
  })),
}))
vi.mock('@/api/tunnel', () => ({
  tunnelApi: { get: vi.fn(async () => ({ tunnel: { state: 'stopped', url: null } })) },
}))

const { ConfigPanel } = await import('./ConfigPanel')

function openPanel(type: string, data: Record<string, unknown>) {
  useFlowWorkflowStore.setState({
    nodes: [{ id: 'n1', type, position: { x: 0, y: 0 }, data }],
    selectedNodeId: 'n1',
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

const stored = () => useFlowWorkflowStore.getState().nodes[0].data

/** The input under a label the panel draws as a sibling, not a <label for>. */
function fieldUnder(label: string): HTMLInputElement {
  const input = screen.getByText(label).parentElement?.querySelector('input')
  if (!input) throw new Error(`no input under ${label}`)
  return input
}

beforeEach(() => {
  useFlowWorkflowStore.getState().resetWorkflow()
  toast.success.mockReset()
  toast.error.mockReset()
})

describe('output names show what is stored (F-585)', () => {
  it.each([
    ['multiQuotes', 'quotes'],
    ['orderBook', 'orders'],
    ['funds', 'funds'],
    ['mathExpression', 'result'],
    ['subscribeLtp', 'ltp'],
  ])('a %s with no name stored shows the name as a hint, not as a value', (type, name) => {
    openPanel(type, {})
    const output = fieldUnder('Output Variable')
    expect(output).toHaveValue('')
    expect(output).toHaveAttribute('placeholder', name)
  })

  it('a stored name shows as the value', () => {
    openPanel('multiQuotes', { outputVariable: 'basket' })
    expect(fieldUnder('Output Variable')).toHaveValue('basket')
  })
})

describe('fallbacks match the run (F-586)', () => {
  it('a split order with no split size shows the run default of 10', () => {
    openPanel('splitOrder', { symbol: 'SBIN', exchange: 'NSE', quantity: 100 })
    expect(fieldUnder('Split Size')).toHaveValue(10)
    expect(screen.getByText(/Splits into\s*10\s*orders/)).toBeInTheDocument()
  })

  it('a history node with no interval shows 5 Min and no forced day range', () => {
    openPanel('history', { symbol: 'SBIN', exchange: 'NSE' })
    expect(screen.getByText('5 Min')).toBeInTheDocument()
    const days = fieldUnder('Days')
    expect(days).toHaveValue(null)
    expect(days).toHaveAttribute('placeholder', 'Broker default')
  })
})

describe('the history interval (F-564)', () => {
  it('stores D for Daily', async () => {
    openPanel('history', { symbol: 'SBIN', exchange: 'NSE', interval: '5m' })
    const user = userEvent.setup()
    await user.click(screen.getByText('5 Min').closest('button') as HTMLElement)
    await user.click(await screen.findByRole('option', { name: 'Daily' }))
    expect(stored().interval).toBe('D')
  })

  it('shows a web export with 1d as stored rather than blank', () => {
    openPanel('history', { symbol: 'SBIN', exchange: 'NSE', interval: '1d' })
    expect(screen.getByText('1d (stored on this node)')).toBeInTheDocument()
  })
})

describe('the variable node (F-605)', () => {
  it('shows the Value box when no operation is stored, as Set Value is shown', () => {
    openPanel('variable', { variableName: 'qty' })
    expect(screen.getByText('Set Value')).toBeInTheDocument()
    expect(screen.getByPlaceholderText('Value or {{variable}}')).toBeInTheDocument()
  })
})

describe('the multi-leg preview (F-545)', () => {
  it('a strangle previews the width it will trade', () => {
    openPanel('optionsMultiOrder', {
      strategy: 'strangle',
      underlying: 'NIFTY',
      action: 'SELL',
      strangleWidth: 'OTM3',
    })
    expect(screen.getByText('OTM3 CE')).toBeInTheDocument()
    expect(screen.getByText('OTM3 PE')).toBeInTheDocument()
    expect(screen.queryByText('OTM2 CE')).not.toBeInTheDocument()
  })

  it('a fixed-side strategy is not captioned long or short', () => {
    openPanel('optionsMultiOrder', { strategy: 'iron_condor', underlying: 'NIFTY', action: 'BUY' })
    expect(screen.queryByText('Long strategy')).not.toBeInTheDocument()
    expect(screen.getByText(/places fixed sides; Action does not change them/)).toBeInTheDocument()
  })

  it('a straddle still follows the action', () => {
    openPanel('optionsMultiOrder', { strategy: 'straddle', underlying: 'NIFTY', action: 'BUY' })
    expect(screen.getByText('Long strategy')).toBeInTheDocument()
  })
})

describe('honest labels', () => {
  it('says the price alert sound and message are not used (F-587)', () => {
    openPanel('priceAlert', { symbol: 'SBIN', exchange: 'NSE', condition: 'above', price: 800 })
    expect(screen.getByText(/neither is used when the alert fires/)).toBeInTheDocument()
    expect(screen.getByText('Play Sound')).toBeInTheDocument()
  })

  it('says bar offset 0 may still be forming (IND-083)', () => {
    openPanel('indicator', { symbol: 'SBIN', exchange: 'NSE', indicatorName: 'rsi' })
    expect(
      screen.getByText(/0 = latest bar, which may still be forming; 1 = the last closed bar/)
    ).toBeInTheDocument()
    expect(screen.queryByText(/latest closed bar/)).not.toBeInTheDocument()
  })
})

describe('schedule days (F-531)', () => {
  it('keeps the chosen days in weekday order', () => {
    openPanel('start', { scheduleType: 'weekly', time: '09:15', days: [6] })
    fireEvent.click(screen.getByRole('button', { name: 'Mon' }))
    fireEvent.click(screen.getByRole('button', { name: 'Wed' }))
    expect(stored().days).toEqual([0, 2, 6])
  })
})

describe('copying the webhook URL (F-512)', () => {
  it('says so when the copy fails instead of claiming it worked', async () => {
    const writeText = vi.spyOn(navigator.clipboard, 'writeText')
    writeText.mockRejectedValueOnce(new Error('denied'))
    openPanel('webhookTrigger', { label: 'TradingView' })
    const copy = await screen.findByRole('button', { name: 'Copy webhook URL' })
    fireEvent.click(copy)
    await waitFor(() => expect(toast.error).toHaveBeenCalledTimes(1))
    expect(toast.success).not.toHaveBeenCalled()
    expect(toast.error.mock.calls[0][0]).toMatch(/Could not copy/)

    fireEvent.click(copy)
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('URL copied'))
    writeText.mockRestore()
    expect(within(document.body).queryByRole('alert')).not.toBeInTheDocument()
  })
})
