/**
 * Pins the config panel's chrome and shared helpers, all the web's code
 * unchanged: parity F-501 (the panel's local option lists), F-503
 * (pruneIndicatorParams on an indicator change), F-504 (the empty state),
 * F-505 (the selected-node header, Delete and Close) and F-506 (every edit is
 * a shallow merge into the node; the Strategy pick writes two keys at once).
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
  getWebhookInfo: vi.fn(async () => null),
}))

const { ConfigPanel } = await import('./ConfigPanel')

const originalUpdateNodeData = useFlowWorkflowStore.getState().updateNodeData

function mountPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })
  return render(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <ConfigPanel />
      </QueryClientProvider>
    </MemoryRouter>
  )
}

function openPanel(type: string, data: Record<string, unknown>) {
  useFlowWorkflowStore.setState({
    nodes: [{ id: 'n1', type, position: { x: 0, y: 0 }, data }],
    selectedNodeId: 'n1',
  })
  return mountPanel()
}

const stored = () => useFlowWorkflowStore.getState().nodes[0].data

/** The first button under a label the panel draws as a sibling: a Select trigger. */
function triggerUnder(label: string): HTMLElement {
  const button = screen
    .getByText(label, { selector: 'label' })
    .parentElement?.querySelector('button')
  if (!button) throw new Error(`no control under ${label}`)
  return button
}

function offered(label: string): string[] {
  fireEvent.click(triggerUnder(label))
  return within(screen.getByRole('listbox'))
    .getAllByRole('option')
    .map((option) => option.textContent ?? '')
}

function choose(label: string, option: string) {
  fireEvent.click(triggerUnder(label))
  fireEvent.click(within(screen.getByRole('listbox')).getByRole('option', { name: option }))
}

/** A `const NAME = [...]` block of ConfigPanel.tsx, one object per entry. */
function sourceList(name: string): Array<Record<string, string>> {
  const panel = readFileSync(
    join(process.cwd(), 'src/components/flow/panels/ConfigPanel.tsx'),
    'utf8'
  )
  const start = panel.indexOf(`const ${name} = [`)
  if (start < 0) throw new Error(`no ${name} in ConfigPanel.tsx`)
  const block = panel.slice(start, panel.indexOf('\n]', start))
  return [...block.matchAll(/\{([^}]*)\}/g)].map((m) =>
    Object.fromEntries([...m[1].matchAll(/(\w+): '([^']*)'/g)].map((f) => [f[1], f[2]]))
  )
}

beforeEach(() => {
  useFlowWorkflowStore.getState().resetWorkflow()
  // Undo an F-506 spy before anything mounts, not after, so no mounted panel re-renders.
  useFlowWorkflowStore.setState({ updateNodeData: originalUpdateNodeData })
})

describe('ALERT_CONDITIONS, ALERT_TRIGGERS, ALERT_EXPIRATION (F-501)', () => {
  const alert = (data: Record<string, unknown> = {}) =>
    openPanel('priceAlert', { symbol: 'SBIN', exchange: 'NSE', ...data })

  it('offers the four conditions, the two triggers and the five expirations in order', () => {
    alert()
    expect(offered('Condition')).toEqual([
      'Greater Than',
      'Less Than',
      'Crosses Above',
      'Crosses Below',
    ])
    fireEvent.keyDown(screen.getByRole('listbox'), { key: 'Escape' })
    expect(offered('Trigger')).toEqual(['Only Once', 'Every Time'])
    fireEvent.keyDown(screen.getByRole('listbox'), { key: 'Escape' })
    expect(offered('Expiration')).toEqual(['No Expiration', '1 Hour', '4 Hours', '1 Day', '1 Week'])
  })

  it.each([
    ['condition', 'above', 'Greater Than', 'Condition'],
    ['condition', 'below', 'Less Than', 'Condition'],
    ['condition', 'crosses_above', 'Crosses Above', 'Condition'],
    ['condition', 'crosses_below', 'Crosses Below', 'Condition'],
    ['trigger', 'once', 'Only Once', 'Trigger'],
    ['trigger', 'every_time', 'Every Time', 'Trigger'],
    ['expiration', 'none', 'No Expiration', 'Expiration'],
    ['expiration', '1h', '1 Hour', 'Expiration'],
    ['expiration', '4h', '4 Hours', 'Expiration'],
    ['expiration', '1d', '1 Day', 'Expiration'],
    ['expiration', '1w', '1 Week', 'Expiration'],
  ])('a stored %s of %s shows %s under %s', (key, value, shown, label) => {
    alert({ [key]: value })
    expect(triggerUnder(label)).toHaveTextContent(shown)
  })

  it('stores the value behind the label picked', () => {
    alert({ condition: 'above', trigger: 'once', expiration: 'none' })
    choose('Condition', 'Crosses Below')
    choose('Trigger', 'Every Time')
    choose('Expiration', '4 Hours')
    expect(stored()).toMatchObject({
      condition: 'crosses_below',
      trigger: 'every_time',
      expiration: '4h',
    })
  })
})

describe('VARIABLE_OPERATIONS (F-501)', () => {
  const OPERATIONS = [
    ['set', 'Set Value', 'Set variable to a value'],
    ['get', 'Get Value', 'Copy from another variable'],
    ['add', 'Add', 'Add to variable'],
    ['subtract', 'Subtract', 'Subtract from variable'],
    ['multiply', 'Multiply', 'Multiply variable'],
    ['divide', 'Divide', 'Divide variable'],
    ['increment', 'Increment', 'Add 1 to variable'],
    ['decrement', 'Decrement', 'Subtract 1 from variable'],
    ['parse_json', 'Parse JSON', 'Parse JSON string to object'],
    ['stringify', 'Stringify', 'Convert to JSON string'],
    ['append', 'Append', 'Append to string'],
  ]

  it('offers the eleven operations in order', () => {
    openPanel('variable', { variableName: 'qty', operation: 'set' })
    expect(offered('Operation')).toEqual(OPERATIONS.map(([, label]) => label))
  })

  it.each(OPERATIONS)('%s shows as %s', (value, label) => {
    openPanel('variable', { variableName: 'qty', operation: value })
    expect(triggerUnder('Operation')).toHaveTextContent(label)
  })

  it('stores the value behind the label picked', () => {
    openPanel('variable', { variableName: 'qty', operation: 'set' })
    choose('Operation', 'Parse JSON')
    expect(stored().operation).toBe('parse_json')
  })

  it('keeps each description as the web has it (not shown in the panel, so read from the source)', () => {
    expect(sourceList('VARIABLE_OPERATIONS')).toEqual(
      OPERATIONS.map(([value, label, description]) => ({ value, label, description }))
    )
  })
})

describe('LOG_LEVELS (F-501)', () => {
  it('offers Info, Warning and Error, each in its colour', () => {
    openPanel('log', { message: 'hi' })
    expect(triggerUnder('Log Level')).toHaveTextContent('Info')
    expect(offered('Log Level')).toEqual(['Info', 'Warning', 'Error'])
    const colour = (name: string) =>
      within(screen.getByRole('option', { name })).getByText(name).className
    expect(colour('Info')).toBe('text-blue-400')
    expect(colour('Warning')).toBe('text-yellow-400')
    expect(colour('Error')).toBe('text-red-400')
    fireEvent.click(screen.getByRole('option', { name: 'Warning' }))
    expect(stored().level).toBe('warn')
  })

  it.each([
    ['warn', 'Warning'],
    ['error', 'Error'],
  ])('%s shows as %s', (level, label) => {
    openPanel('log', { level })
    expect(triggerUnder('Log Level')).toHaveTextContent(label)
  })
})

describe('TIME_OPERATORS (F-501)', () => {
  const OPERATORS = [
    ['==', 'Equals (=)', 'Exactly at this time'],
    ['>=', 'At or After (>=)', 'Time has passed'],
    ['<=', 'At or Before (<=)', 'Before this time'],
    ['>', 'After (>)', 'Strictly after'],
    ['<', 'Before (<)', 'Strictly before'],
  ]

  it('offers the five operators in order, At or After by default', () => {
    openPanel('timeCondition', { targetTime: '09:30' })
    expect(triggerUnder('Operator')).toHaveTextContent('At or After (>=)')
    expect(offered('Operator')).toEqual(OPERATORS.map(([, label]) => label))
  })

  it.each(OPERATORS)('%s shows as %s', (value, label) => {
    openPanel('timeCondition', { operator: value })
    expect(triggerUnder('Operator')).toHaveTextContent(label)
  })

  it('stores the operator behind the label picked', () => {
    openPanel('timeCondition', { operator: '>=' })
    choose('Operator', 'Before (<)')
    expect(stored().operator).toBe('<')
  })

  it('keeps each description as the web has it (not shown in the panel, so read from the source)', () => {
    expect(sourceList('TIME_OPERATORS')).toEqual(
      OPERATORS.map(([value, label, description]) => ({ value, label, description }))
    )
  })
})

describe('CONDITION_TYPES (F-501)', () => {
  it('offers Entry, Exit and Custom as buttons and stores the one clicked', () => {
    openPanel('timeCondition', {})
    const box = screen.getByText('Condition Type').parentElement as HTMLElement
    expect(
      within(box)
        .getAllByRole('button')
        .slice(0, 3)
        .map((b) => b.textContent)
    ).toEqual(['Entry', 'Exit', 'Custom'])

    fireEvent.click(within(box).getByRole('button', { name: 'Exit' }))
    expect(stored().conditionType).toBe('exit')
    expect(within(box).getByRole('button', { name: 'Exit' })).toHaveClass('text-red-600')

    fireEvent.click(within(box).getByRole('button', { name: 'Entry' }))
    expect(stored().conditionType).toBe('entry')
    expect(within(box).getByRole('button', { name: 'Entry' })).toHaveClass('text-green-600')

    fireEvent.click(within(box).getByRole('button', { name: 'Custom' }))
    expect(stored().conditionType).toBe('custom')
    expect(within(box).getByRole('button', { name: 'Custom' })).toHaveClass('bg-primary')
  })
})

describe('HTTP_METHODS (F-501)', () => {
  it('offers GET, POST, PUT, DEL and PATCH, and DEL stores DELETE', () => {
    openPanel('httpRequest', { url: 'https://example.com' })
    const box = screen.getByText('Method').parentElement as HTMLElement
    expect(
      within(box)
        .getAllByRole('button')
        .map((b) => b.textContent)
    ).toEqual(['GET', 'POST', 'PUT', 'DEL', 'PATCH'])
    for (const [label, value] of [
      ['GET', 'GET'],
      ['POST', 'POST'],
      ['PUT', 'PUT'],
      ['DEL', 'DELETE'],
      ['PATCH', 'PATCH'],
    ]) {
      fireEvent.click(within(box).getByRole('button', { name: label }))
      expect(stored().method).toBe(value)
      expect(within(box).getByRole('button', { name: label })).toHaveClass('bg-primary')
    }
  })
})

describe('pruneIndicatorParams on an indicator change (F-503)', () => {
  it.each([
    [
      'keeps only the keys the new indicator takes',
      '{"period": 20, "multiplier": 3}',
      'SMA (Trend)',
      '{"period":20}',
    ],
    [
      'clears params none of which the new indicator takes',
      '{"period": 20}',
      'MACD (Momentum)',
      '',
    ],
    ['leaves an empty params empty', '', 'MACD (Momentum)', ''],
    ['clears whitespace-only params', '   ', 'MACD (Momentum)', ''],
    [
      'leaves malformed JSON for the author to fix',
      '{period: 20',
      'MACD (Momentum)',
      '{period: 20',
    ],
    ['leaves a JSON array alone', '[14]', 'MACD (Momentum)', '[14]'],
    ['leaves JSON null alone', 'null', 'MACD (Momentum)', 'null'],
    [
      'leaves a {{variable}} reference alone',
      '{{tuned.params}}',
      'MACD (Momentum)',
      '{{tuned.params}}',
    ],
  ])(
    '%s',
    (_name, params, indicator, expected) => {
      openPanel('indicator', { symbol: 'SBIN', exchange: 'NSE', indicatorName: 'rsi', params })
      choose('Indicator', indicator)
      expect(stored().params).toBe(expected)
      expect(stored().indicatorName).toBe(indicator.startsWith('SMA') ? 'sma' : 'macd')
    },
    // Opening the indicator list renders every catalogue entry: about a
    // second here, so a slow runner gets room before the 5 s default.
    15_000
  )
})

describe('nothing selected (F-504)', () => {
  it('shows the Properties sidebar and asks for a node', () => {
    useFlowWorkflowStore.setState({ nodes: [], selectedNodeId: null })
    const { container } = mountPanel()

    const sidebar = container.firstElementChild as HTMLElement
    expect(sidebar).toHaveClass('w-80', 'border-l')
    expect(screen.getByText('Properties')).toBeInTheDocument()
    expect(screen.getByText('Select a node to configure')).toBeInTheDocument()
    expect(sidebar.querySelectorAll('svg')).toHaveLength(2)
    expect(screen.queryByRole('heading')).not.toBeInTheDocument()
    expect(screen.queryByRole('button')).not.toBeInTheDocument()
  })

  it('shows the same when the selected id names no node', () => {
    useFlowWorkflowStore.setState({
      nodes: [{ id: 'n1', type: 'log', position: { x: 0, y: 0 }, data: {} }],
      selectedNodeId: 'gone',
    })
    mountPanel()
    expect(screen.getByText('Select a node to configure')).toBeInTheDocument()
  })
})

describe('the selected-node header (F-505)', () => {
  const twoNodes = () =>
    useFlowWorkflowStore.setState({
      nodes: [
        { id: 'n1', type: 'log', position: { x: 0, y: 0 }, data: { message: 'one' } },
        { id: 'n2', type: 'delay', position: { x: 0, y: 0 }, data: {} },
      ],
      edges: [{ id: 'e1', source: 'n2', target: 'n1' }],
      selectedNodeId: 'n1',
    })

  it('titles the node, says Configure node, and scrolls a spaced body', () => {
    const { container } = openPanel('log', { message: 'hi' })

    expect(screen.getByRole('heading', { level: 2 })).toHaveTextContent('Log')
    expect(screen.getByText('Configure node')).toBeInTheDocument()
    expect(container.firstElementChild).toHaveClass('w-80', 'border-l')
    expect(container.querySelector('[data-slot="scroll-area"] .space-y-4.p-4')).not.toBeNull()
  })

  it('deletes the node and its edges at once, with no confirmation', () => {
    twoNodes()
    mountPanel()

    fireEvent.click(screen.getByRole('button', { name: 'Delete node' }))

    const state = useFlowWorkflowStore.getState()
    expect(state.nodes.map((n) => n.id)).toEqual(['n2'])
    expect(state.edges).toEqual([])
    expect(state.selectedNodeId).toBeNull()
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument()
    expect(screen.getByText('Select a node to configure')).toBeInTheDocument()
  })

  it('closes by clearing the selection and keeps the node', () => {
    twoNodes()
    mountPanel()

    fireEvent.click(screen.getByRole('button', { name: 'Close configuration panel' }))

    const state = useFlowWorkflowStore.getState()
    expect(state.selectedNodeId).toBeNull()
    expect(state.nodes.map((n) => n.id)).toEqual(['n1', 'n2'])
    expect(state.edges).toHaveLength(1)
    expect(screen.getByText('Select a node to configure')).toBeInTheDocument()
  })

  it('offers only Delete and Close in the header', () => {
    openPanel('log', { message: 'hi' })
    const header = screen.getByRole('heading', { level: 2 }).closest('.border-b') as HTMLElement
    expect(
      within(header)
        .getAllByRole('button')
        .map((b) => b.getAttribute('aria-label'))
    ).toEqual(['Delete node', 'Close configuration panel'])
  })
})

describe('edits write into the node (F-506)', () => {
  /** Installed before the panel mounts, so the panel's handlers close over it. */
  function spyOnUpdates() {
    const spy = vi.fn(originalUpdateNodeData)
    useFlowWorkflowStore.setState({ updateNodeData: spy })
    return spy
  }

  it('a field edit writes its one key and keeps the rest', () => {
    const spy = spyOnUpdates()
    openPanel('placeOrder', { symbol: 'SBIN', exchange: 'NSE', quantity: 3, action: 'BUY' })

    const symbol = screen.getByText('Symbol').parentElement?.querySelector('input') as HTMLElement
    fireEvent.change(symbol, { target: { value: 'INFY' } })

    expect(spy).toHaveBeenCalledTimes(1)
    expect(spy).toHaveBeenCalledWith('n1', { symbol: 'INFY' })
    expect(stored()).toEqual({ symbol: 'INFY', exchange: 'NSE', quantity: 3, action: 'BUY' })
    expect(useFlowWorkflowStore.getState().isModified).toBe(true)
  })

  it('the Strategy pick leaving custom with a stop price type writes strategy and MARKET in one update', () => {
    const legs = [{ offset: 'ATM', optionType: 'CE', action: 'BUY', quantity: 1 }]
    const spy = spyOnUpdates()
    openPanel('optionsMultiOrder', {
      strategy: 'custom',
      underlying: 'NIFTY',
      priceType: 'SL',
      price: 100,
      triggerPrice: 99,
      legs,
    })

    choose('Strategy', 'Iron Condor')

    expect(spy).toHaveBeenCalledTimes(1)
    expect(spy).toHaveBeenCalledWith('n1', { strategy: 'iron_condor', priceType: 'MARKET' })
    expect(stored()).toEqual({
      strategy: 'iron_condor',
      underlying: 'NIFTY',
      priceType: 'MARKET',
      price: 100,
      triggerPrice: 99,
      legs,
    })
  })

  it('the Strategy pick keeps a LIMIT price type and writes only the strategy', () => {
    const spy = spyOnUpdates()
    openPanel('optionsMultiOrder', { strategy: 'custom', underlying: 'NIFTY', priceType: 'LIMIT' })

    choose('Strategy', 'Straddle')

    expect(spy).toHaveBeenCalledWith('n1', { strategy: 'straddle' })
    expect(stored()).toMatchObject({ strategy: 'straddle', priceType: 'LIMIT' })
  })
})
