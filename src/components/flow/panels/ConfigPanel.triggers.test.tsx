/**
 * Pins the config panel's trigger forms, the web's code unchanged except
 * where noted: parity F-530 (schedule type, time, interval), F-531 (run-on
 * days), F-532 (the once date), F-533 (market hours), F-534 (price alert),
 * F-535 (webhook trigger: label, URL and secret copies, status, unsaved) and
 * F-537 (order update trigger). Labels, placeholders and help sentences are
 * the source's own words; every edit is read back from the store.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { MemoryRouter, Route, Routes } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flowQueryKeys, type WebhookInfo } from '@/api/flow'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

const toast = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn() }))
vi.mock('@/utils/toast', () => ({
  showToast: { success: toast.success, error: toast.error, info: vi.fn(), warning: vi.fn() },
}))

const api = vi.hoisted(() => ({ webhook: vi.fn() }))
vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
  getWebhookInfo: api.webhook,
}))
vi.mock('@/api/tunnel', () => ({
  tunnelApi: { get: vi.fn(async () => ({ tunnel: { state: 'stopped', url: null } })) },
}))

const { ConfigPanel } = await import('./ConfigPanel')

const WEBHOOK_URL = 'https://desk.ngrok-free.app/flow/webhook/wf-token'
const SECRET = 'wf-secret-value'

function webhookInfo(overrides: Partial<WebhookInfo> = {}): WebhookInfo {
  return {
    webhook_token: 'wf-token',
    webhook_secret: SECRET,
    webhook_enabled: true,
    webhook_auth_type: 'payload',
    webhook_url: WEBHOOK_URL,
    webhook_url_with_symbol: `${WEBHOOK_URL}/{symbol}`,
    webhook_url_with_secret: null,
    ...overrides,
  }
}

/** Opens the panel inside /flow/editor/7, or outside any saved workflow. */
function openPanel(type: string, data: Record<string, unknown>, { saved = true } = {}) {
  useFlowWorkflowStore.setState({
    nodes: [{ id: 'n1', type, position: { x: 0, y: 0 }, data }],
    selectedNodeId: 'n1',
  })
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })
  const panel = saved ? (
    <Routes>
      <Route path="/flow/editor/:id" element={<ConfigPanel />} />
    </Routes>
  ) : (
    <ConfigPanel />
  )
  return render(
    <MemoryRouter initialEntries={[saved ? '/flow/editor/7' : '/flow/editor']}>
      <QueryClientProvider client={client}>{panel}</QueryClientProvider>
    </MemoryRouter>
  )
}

const stored = () => useFlowWorkflowStore.getState().nodes[0].data

function labelled(label: string): HTMLElement {
  return screen.getByText(label, { selector: 'label' })
}

/** The input under a label the panel draws as a sibling, not a <label for>. */
function fieldUnder(label: string): HTMLInputElement {
  const input = labelled(label).parentElement?.querySelector('input')
  if (!input) throw new Error(`no input under ${label}`)
  return input
}

function triggerUnder(label: string): HTMLElement {
  const button = labelled(label).parentElement?.querySelector('button')
  if (!button) throw new Error(`no control under ${label}`)
  return button
}

function offered(label: string): string[] {
  fireEvent.click(triggerUnder(label))
  const options = within(screen.getByRole('listbox')).getAllByRole('option')
  const texts = options.map((option) => option.textContent ?? '')
  fireEvent.keyDown(screen.getByRole('listbox'), { key: 'Escape' })
  return texts
}

function choose(label: string, option: string) {
  fireEvent.click(triggerUnder(label))
  fireEvent.click(within(screen.getByRole('listbox')).getByRole('option', { name: option }))
}

const EXCHANGES = [
  'NSE',
  'BSE',
  'NFO',
  'BFO',
  'CDS',
  'BCD',
  'MCX',
  'NCDEX',
  'NCO',
  'NSE_INDEX',
  'BSE_INDEX',
  'MCX_INDEX',
  'GLOBAL_INDEX',
  'CRYPTO',
]

beforeEach(() => {
  useFlowWorkflowStore.getState().resetWorkflow()
  toast.success.mockReset()
  toast.error.mockReset()
  api.webhook.mockReset()
  api.webhook.mockResolvedValue(webhookInfo())
  vi.mocked(navigator.clipboard.writeText).mockClear()
})

describe('start: schedule type, time and interval (F-530)', () => {
  it('offers Once, Daily, Weekly and Interval', () => {
    openPanel('start', { scheduleType: 'daily' })
    expect(triggerUnder('Schedule Type')).toHaveTextContent('Daily')
    expect(offered('Schedule Type')).toEqual(['Once', 'Daily', 'Weekly', 'Interval'])
  })

  // Desktop (web defect): the web showed Daily, but its scheduler and ours
  // read no type, or "manual", as no schedule at all.
  it.each([
    ['no type', {}],
    ['manual', { scheduleType: 'manual' }],
  ])('reads %s as no schedule, with no time or days', (_case, data) => {
    openPanel('start', data)
    expect(triggerUnder('Schedule Type')).toHaveTextContent(
      'No schedule (run it with Run Now or a trigger node)'
    )
    expect(screen.queryByText('Time', { selector: 'label' })).not.toBeInTheDocument()
    expect(screen.queryByText('Run On Days')).not.toBeInTheDocument()
    expect(screen.queryByText('Repeat Every')).not.toBeInTheDocument()
    expect(screen.queryByText('Date', { selector: 'label' })).not.toBeInTheDocument()
  })

  it('stores daily when Daily is picked on a node with no schedule, then shows its fields', () => {
    openPanel('start', {})
    choose('Schedule Type', 'Daily')
    expect(stored().scheduleType).toBe('daily')
    expect(triggerUnder('Schedule Type')).toHaveTextContent('Daily')
    expect(labelled('Time')).toBeInTheDocument()
    expect(labelled('Run On Days')).toBeInTheDocument()
  })

  it.each([
    ['Once', 'once'],
    ['Weekly', 'weekly'],
    ['Interval', 'interval'],
  ])('stores %s as %s', (label, value) => {
    openPanel('start', { scheduleType: 'daily' })
    choose('Schedule Type', label)
    expect(stored().scheduleType).toBe(value)
  })

  it('shows a time box at 09:15 when none is stored and stores the time typed', () => {
    openPanel('start', { scheduleType: 'daily' })
    const time = fieldUnder('Time')
    expect(time).toHaveAttribute('type', 'time')
    expect(time).toHaveValue('09:15')
    fireEvent.change(time, { target: { value: '10:30' } })
    expect(stored().time).toBe('10:30')
  })

  it.each(['once', 'daily', 'weekly'])('shows the time for %s and no repeat', (scheduleType) => {
    openPanel('start', { scheduleType })
    expect(labelled('Time')).toBeInTheDocument()
    expect(screen.queryByText('Repeat Every')).not.toBeInTheDocument()
  })

  it('an interval has no time; it repeats every 1 minute unless told otherwise', () => {
    openPanel('start', { scheduleType: 'interval' })
    expect(screen.queryByText('Time', { selector: 'label' })).not.toBeInTheDocument()
    const every = fieldUnder('Repeat Every')
    expect(every).toHaveAttribute('type', 'number')
    expect(every).toHaveAttribute('min', '1')
    expect(every).toHaveValue(1)
    expect(triggerUnder('Repeat Every')).toHaveTextContent('Minutes')
    expect(offered('Repeat Every')).toEqual(['Seconds', 'Minutes', 'Hours'])
  })

  it.each([
    ['5', 5],
    ['', 1],
    ['0', 1],
  ])('typing %j into Repeat Every stores %s', (typed, value) => {
    openPanel('start', { scheduleType: 'interval', intervalValue: 3 })
    fireEvent.change(fieldUnder('Repeat Every'), { target: { value: typed } })
    expect(stored().intervalValue).toBe(value)
  })

  it('stores the unit picked', () => {
    openPanel('start', { scheduleType: 'interval', intervalValue: 2, intervalUnit: 'minutes' })
    choose('Repeat Every', 'Hours')
    expect(stored().intervalUnit).toBe('hours')
  })
})

describe('start: run-on days (F-531)', () => {
  const day = (name: string) => screen.getByRole('button', { name })
  const selected = () =>
    ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'].filter((name) =>
      day(name).classList.contains('bg-primary')
    )

  it.each(['daily', 'weekly'])('shows the days for %s', (scheduleType) => {
    openPanel('start', { scheduleType })
    expect(labelled('Run On Days')).toBeInTheDocument()
  })

  it.each(['once', 'interval', 'manual', undefined])('has no days for %s', (scheduleType) => {
    openPanel('start', { scheduleType })
    expect(screen.queryByText('Run On Days')).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Mon' })).not.toBeInTheDocument()
  })

  it('selects Monday to Friday when no days are stored, numbered from 0 = Monday', () => {
    openPanel('start', { scheduleType: 'weekly' })
    expect(selected()).toEqual(['Mon', 'Tue', 'Wed', 'Thu', 'Fri'])
    fireEvent.click(day('Sat'))
    expect(stored().days).toEqual([0, 1, 2, 3, 4, 5])
  })

  it('sorts the days as numbers, not as text (the web sorted 10 before 2)', () => {
    // Only a stored value past 9 tells the two apart: a text sort puts "10" first.
    openPanel('start', { scheduleType: 'daily', days: [10] })
    fireEvent.click(day('Wed'))
    expect(stored().days).toEqual([2, 10])
  })

  it('turns a selected day off', () => {
    openPanel('start', { scheduleType: 'daily', days: [0, 2, 4] })
    fireEvent.click(day('Wed'))
    expect(stored().days).toEqual([0, 4])
    expect(selected()).toEqual(['Mon', 'Fri'])
  })

  it.each([
    ['Weekdays', [0, 1, 2, 3, 4]],
    ['Weekends', [5, 6]],
    ['All', [0, 1, 2, 3, 4, 5, 6]],
  ])('%s selects %j', (button, days) => {
    openPanel('start', { scheduleType: 'daily', days: [3] })
    fireEvent.click(day(button))
    expect(stored().days).toEqual(days)
  })
})

describe('start: the date of a once schedule (F-532)', () => {
  it('is an empty date box until a date is stored, and stores the date as YYYY-MM-DD', () => {
    openPanel('start', { scheduleType: 'once' })
    const date = fieldUnder('Date')
    expect(date).toHaveAttribute('type', 'date')
    expect(date).toHaveValue('')
    fireEvent.change(date, { target: { value: '2026-11-03' } })
    expect(stored().executeAt).toBe('2026-11-03')
  })

  it('shows the stored date', () => {
    openPanel('start', { scheduleType: 'once', executeAt: '2026-12-01' })
    expect(fieldUnder('Date')).toHaveValue('2026-12-01')
  })

  it.each(['daily', 'weekly', 'interval'])('has no date for %s', (scheduleType) => {
    openPanel('start', { scheduleType })
    expect(screen.queryByText('Date', { selector: 'label' })).not.toBeInTheDocument()
  })
})

describe('start: market hours (F-533)', () => {
  const HELP =
    "The window narrows the exchange's own session; it never reopens a holiday or a weekend. Leave the calendar on the exchange you trade, so MCX and CRYPTO inherit their real hours."

  it('is on by default, 09:15 to 15:15 on the NSE calendar, with the help sentence', () => {
    openPanel('start', { scheduleType: 'daily' })
    expect(labelled('Only during market hours')).toBeInTheDocument()
    expect(screen.getByRole('switch')).toBeChecked()
    expect(fieldUnder('Start')).toHaveAttribute('type', 'time')
    expect(fieldUnder('Start')).toHaveValue('09:15')
    expect(fieldUnder('End')).toHaveAttribute('type', 'time')
    expect(fieldUnder('End')).toHaveValue('15:15')
    expect(triggerUnder('Calendar')).toHaveTextContent('NSE')
    expect(screen.getByText(HELP)).toBeInTheDocument()
  })

  it('stores the window and the calendar edited', () => {
    openPanel('start', { scheduleType: 'daily', marketHoursOnly: true })
    fireEvent.change(fieldUnder('Start'), { target: { value: '09:30' } })
    fireEvent.change(fieldUnder('End'), { target: { value: '15:00' } })
    choose('Calendar', 'MCX')
    expect(stored()).toMatchObject({
      marketHoursStart: '09:30',
      marketHoursEnd: '15:00',
      marketHoursExchange: 'MCX',
    })
  })

  it('offers every exchange as the calendar', () => {
    openPanel('start', { scheduleType: 'daily' })
    expect(offered('Calendar')).toEqual(EXCHANGES)
  })

  it('switching off stores false and hides the window, the calendar and the help', () => {
    openPanel('start', { scheduleType: 'daily' })
    fireEvent.click(screen.getByRole('switch'))
    expect(stored().marketHoursOnly).toBe(false)
    expect(screen.getByRole('switch')).not.toBeChecked()
    expect(screen.queryByText('Start', { selector: 'label' })).not.toBeInTheDocument()
    expect(screen.queryByText('End', { selector: 'label' })).not.toBeInTheDocument()
    expect(screen.queryByText('Calendar')).not.toBeInTheDocument()
    expect(screen.queryByText(HELP)).not.toBeInTheDocument()
  })

  it('switching back on stores true', () => {
    openPanel('start', { scheduleType: 'daily', marketHoursOnly: false })
    fireEvent.click(screen.getByRole('switch'))
    expect(stored().marketHoursOnly).toBe(true)
    expect(fieldUnder('Start')).toHaveValue('09:15')
  })
})

describe('priceAlert (F-534)', () => {
  it('shows each field with its default when nothing is stored', () => {
    openPanel('priceAlert', {})
    expect(fieldUnder('Symbol')).toHaveAttribute('placeholder', 'RELIANCE')
    expect(fieldUnder('Symbol')).toHaveValue('')
    expect(triggerUnder('Exchange')).toHaveTextContent('NSE')
    expect(triggerUnder('Condition')).toHaveTextContent('Greater Than')
    expect(fieldUnder('Target Price')).toHaveAttribute('type', 'number')
    expect(fieldUnder('Target Price')).toHaveAttribute('step', '0.05')
    expect(fieldUnder('Target Price')).toHaveValue(null)
    expect(triggerUnder('Trigger')).toHaveTextContent('Only Once')
    expect(triggerUnder('Expiration')).toHaveTextContent('No Expiration')
    expect(screen.getByRole('switch')).toBeChecked()
    expect(labelled('Play Sound')).toBeInTheDocument()
    expect(fieldUnder('Alert Message')).toHaveAttribute('placeholder', 'Custom message')
    expect(fieldUnder('Alert Message')).toHaveValue('')
  })

  it('shows a stored target price of 0 as blank', () => {
    openPanel('priceAlert', { symbol: 'SBIN', price: 0 })
    expect(fieldUnder('Target Price')).toHaveValue(null)
  })

  it('stores symbol, exchange, target price, sound and message', () => {
    openPanel('priceAlert', { symbol: '', exchange: 'NSE', price: 0 })
    fireEvent.change(fieldUnder('Symbol'), { target: { value: 'SBIN' } })
    choose('Exchange', 'BSE')
    fireEvent.change(fieldUnder('Target Price'), { target: { value: '812.35' } })
    fireEvent.click(screen.getByRole('switch'))
    fireEvent.change(fieldUnder('Alert Message'), { target: { value: 'SBIN crossed' } })
    expect(stored()).toMatchObject({
      symbol: 'SBIN',
      exchange: 'BSE',
      price: 812.35,
      playSound: false,
      message: 'SBIN crossed',
    })
  })

  it('stores 0 when the target price is cleared', () => {
    openPanel('priceAlert', { symbol: 'SBIN', price: 800 })
    expect(fieldUnder('Target Price')).toHaveValue(800)
    fireEvent.change(fieldUnder('Target Price'), { target: { value: '' } })
    expect(stored().price).toBe(0)
  })

  it('shows a stored playSound false as off', () => {
    openPanel('priceAlert', { symbol: 'SBIN', playSound: false })
    expect(screen.getByRole('switch')).not.toBeChecked()
  })
})

describe('webhookTrigger (F-535)', () => {
  it('stores the label typed', () => {
    openPanel('webhookTrigger', {})
    expect(fieldUnder('Label')).toHaveAttribute('placeholder', 'TradingView Alert')
    fireEvent.change(fieldUnder('Label'), { target: { value: 'Breakout alert' } })
    expect(stored().label).toBe('Breakout alert')
  })

  it('shows a spinner while the webhook settings load', () => {
    api.webhook.mockReturnValue(new Promise(() => {}))
    const { container } = openPanel('webhookTrigger', { label: 'TV' })
    expect(container.querySelector('.animate-spin')).not.toBeNull()
    expect(screen.queryByText('Save workflow first')).not.toBeInTheDocument()
  })

  it('copies the URL and says so', async () => {
    openPanel('webhookTrigger', { label: 'TV' })
    expect(await screen.findByDisplayValue(WEBHOOK_URL)).toHaveAttribute('readonly')
    fireEvent.click(screen.getByRole('button', { name: 'Copy webhook URL' }))
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(WEBHOOK_URL)
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('URL copied'))
  })

  it('copies the URL with /symbol when the node names one (imported JSON)', async () => {
    openPanel('webhookTrigger', { label: 'TV', symbol: 'SBIN' })
    fireEvent.click(await screen.findByRole('button', { name: 'Copy webhook URL' }))
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(`${WEBHOOK_URL}/SBIN`)
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('URL copied'))
  })

  it('masks the secret until the eye is clicked, and hides it again', async () => {
    openPanel('webhookTrigger', { label: 'TV' })
    const secret = await screen.findByDisplayValue(SECRET)
    expect(secret).toHaveAttribute('type', 'password')
    expect(secret).toHaveAttribute('readonly')
    expect(labelled('Webhook Secret')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Show webhook secret' }))
    expect(secret).toHaveAttribute('type', 'text')

    fireEvent.click(screen.getByRole('button', { name: 'Hide webhook secret' }))
    expect(secret).toHaveAttribute('type', 'password')
    expect(screen.getByRole('button', { name: 'Show webhook secret' })).toBeInTheDocument()
  })

  it('copies the secret and says so', async () => {
    openPanel('webhookTrigger', { label: 'TV' })
    fireEvent.click(await screen.findByRole('button', { name: 'Copy webhook secret' }))
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(SECRET)
    await waitFor(() => expect(toast.success).toHaveBeenCalledWith('Secret copied'))
  })

  it('says the webhook is enabled, in green', async () => {
    openPanel('webhookTrigger', { label: 'TV' })
    const pill = await screen.findByText('Webhook enabled')
    expect(pill).toHaveClass('text-green-600')
    expect(screen.queryByText('Webhook disabled')).not.toBeInTheDocument()
  })

  it('says the webhook is disabled, in yellow', async () => {
    api.webhook.mockResolvedValue(webhookInfo({ webhook_enabled: false }))
    openPanel('webhookTrigger', { label: 'TV' })
    const pill = await screen.findByText('Webhook disabled')
    expect(pill).toHaveClass('text-yellow-600')
    expect(screen.queryByText('Webhook enabled')).not.toBeInTheDocument()
  })

  it('offers no control that changes the webhook itself', async () => {
    openPanel('webhookTrigger', { label: 'TV' })
    await screen.findByText('Webhook enabled')
    expect(screen.queryByRole('switch')).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /regenerate|enable|disable/i })).toBeNull()
  })

  it('asks to save the workflow first when it has no id yet', () => {
    openPanel('webhookTrigger', { label: 'TV' }, { saved: false })
    expect(screen.getByText('Save workflow first')).toBeInTheDocument()
    expect(api.webhook).not.toHaveBeenCalled()
    expect(screen.queryByText('Webhook URL')).not.toBeInTheDocument()
  })

  it('asks to save the workflow first when its settings cannot be read', async () => {
    api.webhook.mockRejectedValue(new Error('not found'))
    openPanel('webhookTrigger', { label: 'TV' })
    expect(await screen.findByText('Save workflow first')).toBeInTheDocument()
    expect(api.webhook).toHaveBeenCalledWith(7)
  })
})

describe('orderUpdateTrigger (F-537)', () => {
  const ORDER_ID_HELP =
    'A literal broker order id. {{variable}} references are not supported here - a trigger has no upstream node to resolve them from. To react to an order this workflow placed, filter by Symbol instead.'
  const FILTER_NOTE =
    'Set at least one of Order ID / Symbol - an unfiltered watch would fire on every order in the account.'

  it('shows its fields, help and defaults', () => {
    openPanel('orderUpdateTrigger', { orderId: '', symbol: '', exchange: '' })
    expect(fieldUnder('Order ID (optional)')).toHaveAttribute('placeholder', '240221025997024')
    expect(screen.getByText(ORDER_ID_HELP)).toBeInTheDocument()
    expect(fieldUnder('Symbol (optional)')).toHaveAttribute('placeholder', 'NIFTY28OCT2525950CE')
    expect(screen.getByText(FILTER_NOTE)).toBeInTheDocument()
    expect(triggerUnder('Exchange (optional)')).toHaveTextContent('Any exchange')
    expect(triggerUnder('Status')).toHaveTextContent('Complete (filled)')
    expect(triggerUnder('Trigger')).toHaveTextContent('Once, then stop watching')
  })

  it('stores the order id and symbol typed', () => {
    openPanel('orderUpdateTrigger', {})
    fireEvent.change(fieldUnder('Order ID (optional)'), { target: { value: '2510280001' } })
    fireEvent.change(fieldUnder('Symbol (optional)'), { target: { value: 'SBIN' } })
    expect(stored()).toMatchObject({ orderId: '2510280001', symbol: 'SBIN' })
  })

  it('offers any exchange then every exchange, and stores Any exchange as empty', () => {
    openPanel('orderUpdateTrigger', { exchange: '' })
    expect(offered('Exchange (optional)')).toEqual(['Any exchange', ...EXCHANGES])
    choose('Exchange (optional)', 'NFO')
    expect(stored().exchange).toBe('NFO')
    choose('Exchange (optional)', 'Any exchange')
    expect(stored().exchange).toBe('')
  })

  it('offers the six statuses and stores the value behind each', () => {
    openPanel('orderUpdateTrigger', { status: 'complete' })
    expect(offered('Status')).toEqual([
      'Any status change',
      'Open',
      'Trigger Pending',
      'Complete (filled)',
      'Rejected',
      'Cancelled',
    ])
    for (const [label, value] of [
      ['Any status change', 'any'],
      ['Open', 'open'],
      ['Trigger Pending', 'trigger pending'],
      ['Rejected', 'rejected'],
      ['Cancelled', 'cancelled'],
      ['Complete (filled)', 'complete'],
    ]) {
      choose('Status', label)
      expect(stored().status).toBe(value)
    }
  })

  it('offers once or every update and stores the value behind each', () => {
    openPanel('orderUpdateTrigger', { trigger: 'once' })
    expect(offered('Trigger')).toEqual(['Once, then stop watching', 'Every matching update'])
    choose('Trigger', 'Every matching update')
    expect(stored().trigger).toBe('every_time')
    choose('Trigger', 'Once, then stop watching')
    expect(stored().trigger).toBe('once')
  })
})

describe('F-509: only a webhook trigger asks for the webhook, under flowQueryKeys.webhook(id)', () => {
  const ORDER_NODE = { id: 'n1', type: 'placeOrder', position: { x: 0, y: 0 }, data: {} }
  const WEBHOOK_NODE = {
    id: 'n2',
    type: 'webhookTrigger',
    position: { x: 0, y: 0 },
    data: { label: 'TV' },
  }

  /** The panel inside /flow/editor/7 on a client the test can read. */
  function mountSaved(client: QueryClient) {
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

  const newClient = () =>
    new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })

  it('asks for nothing while another node is open in a saved workflow', async () => {
    useFlowWorkflowStore.setState({ nodes: [ORDER_NODE, WEBHOOK_NODE], selectedNodeId: 'n1' })
    const client = newClient()
    mountSaved(client)
    await act(async () => {})
    expect(screen.getByRole('heading', { level: 2 })).toHaveTextContent('Place Order')
    expect(api.webhook).not.toHaveBeenCalled()
    expect(client.getQueryState(flowQueryKeys.webhook(7))?.fetchStatus).toBe('idle')

    act(() => useFlowWorkflowStore.getState().selectNode('n2'))
    expect(await screen.findByDisplayValue(WEBHOOK_URL)).toBeInTheDocument()
    expect(api.webhook).toHaveBeenCalledTimes(1)
    expect(api.webhook).toHaveBeenCalledWith(7)
  })

  it('keeps the answer under flowQueryKeys.webhook(7), so refreshing that key shows a new URL', async () => {
    useFlowWorkflowStore.setState({ nodes: [WEBHOOK_NODE], selectedNodeId: 'n2' })
    const client = newClient()
    mountSaved(client)
    await screen.findByDisplayValue(WEBHOOK_URL)
    expect(client.getQueryState(flowQueryKeys.webhook(7))?.status).toBe('success')

    const regenerated = 'https://desk.ngrok-free.app/flow/webhook/new-token'
    api.webhook.mockResolvedValue(webhookInfo({ webhook_url: regenerated }))
    await act(() => client.invalidateQueries({ queryKey: flowQueryKeys.webhook(7) }))
    expect(await screen.findByDisplayValue(regenerated)).toBeInTheDocument()
    expect(api.webhook).toHaveBeenCalledTimes(2)
  })
})

describe('F-530: the Seconds unit of an interval schedule', () => {
  it.each([
    ['seconds', 'Seconds'],
    ['minutes', 'Minutes'],
    ['hours', 'Hours'],
  ])('a stored %s shows %s', (intervalUnit, shown) => {
    openPanel('start', { scheduleType: 'interval', intervalValue: 2, intervalUnit })
    expect(triggerUnder('Repeat Every')).toHaveTextContent(new RegExp(`^${shown}$`))
  })

  it.each([
    ['Seconds', 'seconds', 'hours'],
    ['Minutes', 'minutes', 'seconds'],
  ])('picking %s stores %s', (label, value, from) => {
    openPanel('start', { scheduleType: 'interval', intervalValue: 2, intervalUnit: from })
    choose('Repeat Every', label)
    expect(stored().intervalUnit).toBe(value)
  })
})

describe('F-535: the Webhook URL label', () => {
  it('names the URL box Webhook URL', async () => {
    openPanel('webhookTrigger', { label: 'TV' })
    await screen.findByDisplayValue(WEBHOOK_URL)
    expect(fieldUnder('Webhook URL')).toHaveValue(WEBHOOK_URL)
  })
})
