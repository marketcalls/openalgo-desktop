/**
 * Pins the config panel's options forms, the web's code unchanged except the
 * F-545 preview and caption fixes: parity F-510 (index lot sizes feed the lot
 * preview), F-511 (an underlying sets its exchange; MCX keeps monthly expiries
 * only), F-542 (options order), F-543 to F-545 (multi-leg strategy, notes and
 * legs preview) and F-546 (what the panel hands the custom-legs editor).
 * CustomLegsFields has its own tests; here it is replaced by a stand-in that
 * records its props.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, fireEvent, render, screen, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { IndexSymbolInfo } from '@/api/flow'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

const api = vi.hoisted(() => ({ lots: vi.fn() }))
vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: api.lots,
  getWebhookInfo: vi.fn(async () => null),
}))

const legsEditor = vi.hoisted(() => ({ props: null as Record<string, unknown> | null }))
vi.mock('./CustomLegsFields', () => ({
  CustomLegsFields: (props: Record<string, unknown>) => {
    legsEditor.props = props
    return <div data-testid="custom-legs" />
  },
}))

const { ConfigPanel } = await import('./ConfigPanel')

const NIFTY_LOT: IndexSymbolInfo = { value: 'NIFTY', label: 'NIFTY', exchange: 'NFO', lotSize: 75 }

function openPanel(type: string, data: Record<string, unknown>) {
  useFlowWorkflowStore.setState({
    nodes: [{ id: 'n1', type, position: { x: 0, y: 0 }, data }],
    selectedNodeId: 'n1',
  })
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })
  return render(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <ConfigPanel />
      </QueryClientProvider>
    </MemoryRouter>
  )
}

const stored = () => useFlowWorkflowStore.getState().nodes[0].data
const labelled = (label: string) => screen.getByText(label, { selector: 'label' })

function fieldUnder(label: string): HTMLInputElement {
  const input = labelled(label).parentElement?.querySelector('input')
  if (!input) throw new Error(`no input under ${label}`)
  return input
}

/** The first button under a label: a Select trigger, never the Braces toggle. */
function triggerUnder(label: string): HTMLElement {
  const button = labelled(label).parentElement?.querySelector('button')
  if (!button) throw new Error(`no control under ${label}`)
  return button
}

function offered(label: string): string[] {
  fireEvent.click(triggerUnder(label))
  const texts = within(screen.getByRole('listbox'))
    .getAllByRole('option')
    .map((option) => option.textContent ?? '')
  fireEvent.keyDown(screen.getByRole('listbox'), { key: 'Escape' })
  return texts
}

function choose(label: string, option: string) {
  fireEvent.click(triggerUnder(label))
  fireEvent.click(within(screen.getByRole('listbox')).getByRole('option', { name: option }))
}

/** The lot preview line, read whole: "{n} lot(s) x {size} = {units} units". */
const lotPreview = () =>
  screen.queryByText((_, el) => el?.tagName === 'P' && / lots? x \d+ = /.test(el.textContent ?? ''))

/** Resolves once the panel's index lot-size request has answered and rendered. */
async function lotSizesLoaded() {
  await vi.waitFor(() => expect(api.lots).toHaveBeenCalled())
  await act(async () => {
    await api.lots.mock.results[0]?.value
  })
}

/** Each row of the Strategy Legs preview as [leg, side, side colour]. */
function previewRows(): Array<[string, string, string]> {
  const box = screen.getByText('Strategy Legs:').nextElementSibling as HTMLElement
  return [...box.querySelectorAll(':scope > div')].map((row) => {
    const [leg, side] = [...row.querySelectorAll('span')]
    return [leg.textContent ?? '', side.textContent ?? '', side.className]
  })
}

const UNDERLYINGS = [
  'NIFTY (NFO)',
  'BANKNIFTY (NFO)',
  'FINNIFTY (NFO)',
  'MIDCPNIFTY (NFO)',
  'NIFTYNXT50 (NFO)',
  'SENSEX (BFO)',
  'BANKEX (BFO)',
  'SENSEX50 (BFO)',
  'GOLD (MCX)',
  'GOLDM (MCX)',
  'SILVER (MCX)',
  'SILVERM (MCX)',
  'CRUDEOIL (MCX)',
  'CRUDEOILM (MCX)',
  'NATURALGAS (MCX)',
  'NATGASMINI (MCX)',
  'COPPER (MCX)',
  'ZINC (MCX)',
  'MCXBULLDEX (MCX)',
]

const OPTION = {
  underlying: 'NIFTY',
  exchange: 'NSE_INDEX',
  expiryType: 'current_week',
  offset: 'ATM',
  action: 'BUY',
  quantity: 2,
}

beforeEach(() => {
  useFlowWorkflowStore.getState().resetWorkflow()
  api.lots.mockReset()
  api.lots.mockResolvedValue([NIFTY_LOT])
  legsEditor.props = null
})

describe('index lot sizes feed the lot preview (F-510)', () => {
  it('asks for the index lot sizes once the panel opens, whatever the node', () => {
    openPanel('placeOrder', { symbol: 'SBIN' })
    expect(api.lots).toHaveBeenCalledTimes(1)
  })

  it('multiplies the lots by the lot size the database has', async () => {
    openPanel('optionsOrder', OPTION)
    expect(await screen.findByText('150 units')).toBeInTheDocument()
    expect(lotPreview()?.textContent).toBe('2 lots x 75 = 150 units')
  })

  it('says lot, not lots, for one', async () => {
    openPanel('optionsOrder', { ...OPTION, quantity: 1 })
    expect(await screen.findByText('75 units')).toBeInTheDocument()
    expect(lotPreview()?.textContent).toBe('1 lot x 75 = 75 units')
  })

  it('counts one lot when no quantity is stored', async () => {
    const { quantity: _omit, ...noQuantity } = OPTION
    openPanel('optionsOrder', noQuantity)
    expect(await screen.findByText('75 units')).toBeInTheDocument()
    expect(fieldUnder('Quantity (Lots)')).toHaveValue(1)
  })

  it('shows no preview for an underlying the database has no lot size for', async () => {
    openPanel('optionsOrder', { ...OPTION, underlying: 'BANKNIFTY' })
    await lotSizesLoaded()
    expect(lotPreview()).toBeNull()
  })

  it('shows no preview while the quantity is a {{reference}}', async () => {
    openPanel('optionsOrder', { ...OPTION, quantity: '{{webhook.quantity}}' })
    await lotSizesLoaded()
    expect(lotPreview()).toBeNull()
  })
})

describe('the underlying sets the exchange and the expiry choices (F-511)', () => {
  it.each([
    ['BANKNIFTY (NFO)', 'BANKNIFTY', 'NFO'],
    ['SENSEX (BFO)', 'SENSEX', 'BFO'],
  ])('%s stores %s on %s and keeps a weekly expiry', (option, underlying, exchange) => {
    openPanel('optionsOrder', OPTION)
    choose('Underlying', option)
    expect(stored()).toMatchObject({ underlying, exchange, expiryType: 'current_week' })
  })

  it('an MCX underlying moves a weekly expiry to the current month', () => {
    openPanel('optionsOrder', { ...OPTION, expiryType: 'next_week' })
    choose('Underlying', 'GOLD (MCX)')
    expect(stored()).toMatchObject({
      underlying: 'GOLD',
      exchange: 'MCX',
      expiryType: 'current_month',
    })
  })

  it('an MCX underlying moves the default weekly expiry when none is stored', () => {
    const { expiryType: _omit, ...noExpiry } = OPTION
    openPanel('optionsOrder', noExpiry)
    choose('Underlying', 'CRUDEOIL (MCX)')
    expect(stored().expiryType).toBe('current_month')
  })

  it('an MCX underlying keeps a monthly expiry', () => {
    openPanel('optionsOrder', { ...OPTION, expiryType: 'next_month' })
    choose('Underlying', 'SILVER (MCX)')
    expect(stored()).toMatchObject({ exchange: 'MCX', expiryType: 'next_month' })
  })

  it('offers the four expiries on an index and only the monthly two on MCX', () => {
    openPanel('optionsOrder', OPTION)
    expect(offered('Expiry')).toEqual(['Current Week', 'Next Week', 'Current Month', 'Next Month'])
    choose('Underlying', 'GOLD (MCX)')
    expect(triggerUnder('Expiry')).toHaveTextContent('Current Month')
    expect(offered('Expiry')).toEqual(['Current Month', 'Next Month'])
  })

  it('applies the same rule on the multi-leg node', () => {
    openPanel('optionsMultiOrder', {
      strategy: 'straddle',
      underlying: 'NIFTY',
      expiryType: 'current_week',
    })
    choose('Underlying', 'NATURALGAS (MCX)')
    expect(stored()).toMatchObject({
      underlying: 'NATURALGAS',
      exchange: 'MCX',
      expiryType: 'current_month',
    })
    expect(offered('Expiry')).toEqual(['Current Month', 'Next Month'])
  })
})

describe('optionsOrder (F-542)', () => {
  it('lays out the option fields in the web order', () => {
    openPanel('optionsOrder', OPTION)
    const labels = [...document.querySelectorAll('.space-y-4.p-4 label')].map((l) => l.textContent)
    expect(labels).toEqual([
      'Underlying',
      'Expiry',
      'Strike Offset',
      'Option Type',
      'Action',
      'Quantity (Lots)',
      'Product',
      'Price Type',
      'Output Variable',
      'Node ID',
    ])
  })

  it('offers every index and commodity underlying, NIFTY when none is stored', () => {
    const { underlying: _omit, ...noUnderlying } = OPTION
    openPanel('optionsOrder', noUnderlying)
    expect(triggerUnder('Underlying')).toHaveTextContent('NIFTY (NFO)')
    expect(offered('Underlying')).toEqual(UNDERLYINGS)
  })

  it('never writes the lot size into the lot count', async () => {
    api.lots.mockResolvedValue([NIFTY_LOT, { ...NIFTY_LOT, value: 'BANKNIFTY', lotSize: 35 }])
    openPanel('optionsOrder', OPTION)
    await screen.findByText('150 units')
    choose('Underlying', 'BANKNIFTY (NFO)')
    expect(stored().quantity).toBe(2)
    expect(await screen.findByText('70 units')).toBeInTheDocument()
  })

  it('shows Current Week and ATM when no expiry or offset is stored', () => {
    openPanel('optionsOrder', { underlying: 'NIFTY' })
    expect(triggerUnder('Expiry')).toHaveTextContent('Current Week')
    expect(triggerUnder('Strike Offset')).toHaveTextContent('ATM')
  })

  it('takes the strike offset from the payload, says what it must be, and falls back to ATM', () => {
    openPanel('optionsOrder', { ...OPTION, offset: 'OTM2' })
    const hint = 'Must resolve to ATM, ITM1-ITM50 or OTM1-OTM50.'
    expect(screen.queryByText(hint)).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Take Strike Offset from the payload' }))
    expect(stored().offset).toBe('{{webhook.offset}}')
    expect(screen.getByText(hint)).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Use a fixed Strike Offset' }))
    expect(stored().offset).toBe('ATM')
  })

  it('stores the offset picked', () => {
    openPanel('optionsOrder', OPTION)
    choose('Strike Offset', 'ITM3')
    expect(stored().offset).toBe('ITM3')
  })

  it('offers Call (CE) and Put (PE) with neither chosen until one is clicked', () => {
    openPanel('optionsOrder', OPTION)
    const call = screen.getByRole('button', { name: 'Call (CE)' })
    const put = screen.getByRole('button', { name: 'Put (PE)' })
    expect(call).toHaveClass('bg-muted')
    expect(put).toHaveClass('bg-muted')
    expect(stored().optionType).toBeUndefined()

    fireEvent.click(put)
    expect(stored().optionType).toBe('PE')
    expect(screen.getByRole('button', { name: 'Put (PE)' })).toHaveClass('text-red-600')
    fireEvent.click(screen.getByRole('button', { name: 'Call (CE)' }))
    expect(stored().optionType).toBe('CE')
    expect(screen.getByRole('button', { name: 'Call (CE)' })).toHaveClass('text-green-600')
  })

  it('takes the option type from the payload, says CE or PE, and falls back to CE', () => {
    openPanel('optionsOrder', { ...OPTION, optionType: 'PE' })
    fireEvent.click(screen.getByRole('button', { name: 'Take Option Type from the payload' }))
    expect(stored().optionType).toBe('{{webhook.optionType}}')
    expect(screen.getByText('Must resolve to CE or PE.')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Use a fixed Option Type' }))
    expect(stored().optionType).toBe('CE')
  })

  it('counts the quantity in lots and stores the count typed', () => {
    openPanel('optionsOrder', OPTION)
    expect(fieldUnder('Quantity (Lots)')).toHaveValue(2)
    fireEvent.change(fieldUnder('Quantity (Lots)'), { target: { value: '4' } })
    expect(stored().quantity).toBe(4)
    fireEvent.click(screen.getByRole('button', { name: 'SELL' }))
    expect(stored().action).toBe('SELL')
  })

  it('carries the price fields and names the result optionOrder', () => {
    openPanel('optionsOrder', { ...OPTION, priceType: 'SL' })
    expect(triggerUnder('Price Type')).toHaveTextContent('Stop Loss Limit')
    expect(screen.getByLabelText('Price')).toBeInTheDocument()
    expect(screen.getByLabelText('Trigger Price')).toBeInTheDocument()
    expect(fieldUnder('Output Variable')).toHaveAttribute('placeholder', 'optionOrder')
  })
})

describe('optionsMultiOrder: strategy, action and quantity (F-543)', () => {
  const MULTI = { strategy: 'straddle', underlying: 'NIFTY', expiryType: 'current_week' }

  it('offers the six strategies, Custom when none is stored (the run default)', () => {
    openPanel('optionsMultiOrder', { underlying: 'NIFTY' })
    expect(triggerUnder('Strategy')).toHaveTextContent('Custom')
    expect(offered('Strategy')).toEqual([
      'Iron Condor',
      'Straddle',
      'Strangle',
      'Bull Call Spread',
      'Bear Put Spread',
      'Custom',
    ])
  })

  it.each([
    ['Iron Condor', 'iron_condor'],
    ['Strangle', 'strangle'],
    ['Bull Call Spread', 'bull_call_spread'],
    ['Bear Put Spread', 'bear_put_spread'],
    ['Custom', 'custom'],
  ])('stores %s as %s and keeps the price type', (label, value) => {
    openPanel('optionsMultiOrder', { ...MULTI, priceType: 'LIMIT' })
    choose('Strategy', label)
    expect(stored()).toMatchObject({ strategy: value, priceType: 'LIMIT' })
  })

  it('lays out the strategy fields in the web order', () => {
    openPanel('optionsMultiOrder', MULTI)
    const labels = [...document.querySelectorAll('.space-y-4.p-4 label')].map((l) => l.textContent)
    expect(labels).toEqual([
      'Strategy',
      'Underlying',
      'Expiry',
      'Action',
      'Quantity (Lots)',
      'Product',
      'Price Type',
      'Output Variable',
      'Node ID',
    ])
  })

  it('has a fixed BUY/SELL pair for Action, with no payload toggle', () => {
    openPanel('optionsMultiOrder', MULTI)
    expect(screen.queryByRole('button', { name: 'Take Action from the payload' })).toBeNull()
    expect(screen.getByText('Short strategy')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'BUY' }))
    expect(stored().action).toBe('BUY')
    expect(screen.getByRole('button', { name: 'BUY' })).toHaveClass('text-green-600')
    expect(screen.getByText('Long strategy')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'SELL' }))
    expect(stored().action).toBe('SELL')
    expect(screen.getByRole('button', { name: 'SELL' })).toHaveClass('text-red-600')
    expect(screen.getByText('Short strategy')).toBeInTheDocument()
  })

  it('counts the quantity in lots, 1 when none is stored, and stores the count typed', () => {
    openPanel('optionsMultiOrder', MULTI)
    expect(fieldUnder('Quantity (Lots)')).toHaveValue(1)
    fireEvent.change(fieldUnder('Quantity (Lots)'), { target: { value: '3' } })
    expect(stored().quantity).toBe(3)
  })

  it('stores the expiry picked and carries the price fields', () => {
    openPanel('optionsMultiOrder', { ...MULTI, priceType: 'SL-M' })
    choose('Expiry', 'Next Week')
    expect(stored().expiryType).toBe('next_week')
    expect(screen.getByLabelText('Trigger Price')).toBeInTheDocument()
    expect(fieldUnder('Output Variable')).toHaveAttribute('placeholder', 'multiLegOrder')
  })
})

describe('optionsMultiOrder: the pricing notes (F-544)', () => {
  const CUSTOM_NOTE =
    "Custom legs inherit these common product and price fields when omitted. A leg's explicit product, price type, price, or trigger price overrides the common value."
  const LIMIT_NOTE =
    'Applied to every generated leg. A LIMIT order without a positive price is rejected rather than sent at market.'

  it('a custom strategy says how its legs inherit the common fields', () => {
    openPanel('optionsMultiOrder', { strategy: 'custom', underlying: 'NIFTY', legs: [] })
    expect(screen.getByText(CUSTOM_NOTE)).toBeInTheDocument()
    expect(screen.queryByText(LIMIT_NOTE)).not.toBeInTheDocument()
  })

  it('a LIMIT preset says the price applies to every leg', () => {
    openPanel('optionsMultiOrder', {
      strategy: 'straddle',
      underlying: 'NIFTY',
      priceType: 'LIMIT',
    })
    expect(screen.getByText(LIMIT_NOTE)).toBeInTheDocument()
    expect(screen.queryByText(CUSTOM_NOTE)).not.toBeInTheDocument()
  })

  it('a LIMIT custom strategy shows only the custom note', () => {
    openPanel('optionsMultiOrder', { strategy: 'custom', underlying: 'NIFTY', priceType: 'LIMIT' })
    expect(screen.getByText(CUSTOM_NOTE)).toBeInTheDocument()
    expect(screen.queryByText(LIMIT_NOTE)).not.toBeInTheDocument()
  })

  it.each(['MARKET', 'SL', 'SL-M'])('a %s preset shows neither note', (priceType) => {
    openPanel('optionsMultiOrder', { strategy: 'iron_condor', underlying: 'NIFTY', priceType })
    expect(screen.queryByText(CUSTOM_NOTE)).not.toBeInTheDocument()
    expect(screen.queryByText(LIMIT_NOTE)).not.toBeInTheDocument()
  })
})

describe('optionsMultiOrder: the legs preview (F-545)', () => {
  const GREEN = 'text-green-600'
  const RED = 'text-red-600'
  const multi = (data: Record<string, unknown>) =>
    openPanel('optionsMultiOrder', { underlying: 'NIFTY', ...data })

  it('a straddle sells ATM CE and PE when no action is stored', () => {
    multi({ strategy: 'straddle' })
    expect(previewRows()).toEqual([
      ['ATM CE', 'SELL', RED],
      ['ATM PE', 'SELL', RED],
    ])
  })

  it('a straddle follows a BUY action, in green', () => {
    multi({ strategy: 'straddle', action: 'BUY' })
    expect(previewRows()).toEqual([
      ['ATM CE', 'BUY', GREEN],
      ['ATM PE', 'BUY', GREEN],
    ])
  })

  it('a strangle with no width previews OTM2 with the node action', () => {
    multi({ strategy: 'strangle', action: 'BUY' })
    expect(previewRows()).toEqual([
      ['OTM2 CE', 'BUY', GREEN],
      ['OTM2 PE', 'BUY', GREEN],
    ])
  })

  it('an iron condor previews its four fixed legs whatever the action', () => {
    multi({ strategy: 'iron_condor', action: 'BUY' })
    expect(previewRows()).toEqual([
      ['OTM2 CE', 'SELL', RED],
      ['OTM4 CE', 'BUY', GREEN],
      ['OTM2 PE', 'SELL', RED],
      ['OTM4 PE', 'BUY', GREEN],
    ])
  })

  it('a bull call spread buys ATM CE and sells OTM2 CE whatever the action', () => {
    multi({ strategy: 'bull_call_spread', action: 'SELL' })
    expect(previewRows()).toEqual([
      ['ATM CE', 'BUY', GREEN],
      ['OTM2 CE', 'SELL', RED],
    ])
  })

  it('a bear put spread buys ATM PE and sells OTM2 PE whatever the action', () => {
    multi({ strategy: 'bear_put_spread', action: 'SELL' })
    expect(previewRows()).toEqual([
      ['ATM PE', 'BUY', GREEN],
      ['OTM2 PE', 'SELL', RED],
    ])
  })

  it('a custom strategy says its legs are built below', () => {
    multi({ strategy: 'custom', legs: [] })
    expect(previewRows()).toEqual([])
    expect(screen.getByText('Built below, leg by leg.')).toBeInTheDocument()
  })

  it.each(['bull_call_spread', 'bear_put_spread'])(
    'a %s is captioned as fixed-sided, not long or short',
    (strategy) => {
      multi({ strategy, action: 'SELL' })
      expect(screen.queryByText('Short strategy')).not.toBeInTheDocument()
      expect(
        screen.getByText('This strategy places fixed sides; Action does not change them.')
      ).toBeInTheDocument()
    }
  )
})

describe('optionsMultiOrder: the custom-legs editor (F-546)', () => {
  it('is only shown for a custom strategy', () => {
    openPanel('optionsMultiOrder', { strategy: 'straddle', underlying: 'NIFTY' })
    expect(screen.queryByTestId('custom-legs')).not.toBeInTheDocument()
    expect(legsEditor.props).toBeNull()
  })

  it('hands the editor the node defaults when the node stores none', () => {
    openPanel('optionsMultiOrder', { strategy: 'custom' })
    expect(screen.getByTestId('custom-legs')).toBeInTheDocument()
    expect(legsEditor.props).toMatchObject({
      value: undefined,
      commonPriceType: 'MARKET',
      commonProduct: 'NRML',
      commonExpiryType: 'current_week',
      commonAction: 'SELL',
      commonQuantity: 1,
      strangleWidth: 'OTM2',
      underlying: 'NIFTY',
    })
  })

  it('hands the editor what the node stores', () => {
    const legs = [{ offset: 'ATM', optionType: 'CE', action: 'BUY', quantity: 1 }]
    openPanel('optionsMultiOrder', {
      strategy: 'custom',
      legs,
      priceType: 'LIMIT',
      product: 'MIS',
      expiryType: 'next_month',
      action: 'BUY',
      quantity: 3,
      strangleWidth: 'OTM4',
      underlying: 'BANKNIFTY',
    })
    expect(legsEditor.props).toMatchObject({
      value: legs,
      commonPriceType: 'LIMIT',
      commonProduct: 'MIS',
      commonExpiryType: 'next_month',
      commonAction: 'BUY',
      commonQuantity: 3,
      strangleWidth: 'OTM4',
      underlying: 'BANKNIFTY',
    })
  })

  it('stores the legs the editor hands back, and is followed by the multiLegOrder output', () => {
    openPanel('optionsMultiOrder', { strategy: 'custom', legs: [] })
    const next = [{ offset: 'OTM1', optionType: 'PE', action: 'SELL', quantity: 2 }]
    const onChange = legsEditor.props?.onChange as (legs: unknown) => void
    act(() => onChange(next))
    expect(stored().legs).toEqual(next)
    const output = fieldUnder('Output Variable')
    expect(output).toHaveAttribute('placeholder', 'multiLegOrder')
    expect(
      screen.getByTestId('custom-legs').compareDocumentPosition(output) &
        Node.DOCUMENT_POSITION_FOLLOWING
    ).toBeTruthy()
  })
})
