/**
 * Parity F-586: what the config panel shows for a field the node does not
 * store must be what the run uses for it. Each fallback is read from the
 * executor's own source (src-tauri/src/flow/nodes), so a default changed on
 * either side fails here. Nodes the editor creates store DEFAULT_NODE_DATA,
 * so this matters for imported and hand-written workflows.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
}))

const { ConfigPanel } = await import('./ConfigPanel')

const NODES = join(process.cwd(), 'src-tauri/src/flow/nodes')
const rust = (file: string) => readFileSync(join(NODES, file), 'utf8')

/** The first capture of `pattern` in a node source file. */
function executorDefault(file: string, pattern: RegExp): string {
  const m = rust(file).match(pattern)
  if (!m) throw new Error(`${pattern} not found in ${file}`)
  return m[1]
}

function openPanel(type: string, data: Record<string, unknown> = {}) {
  useFlowWorkflowStore.setState({
    nodes: [{ id: 'n1', type, position: { x: 0, y: 0 }, data }],
    selectedNodeId: 'n1',
  })
  render(
    <MemoryRouter>
      <QueryClientProvider client={new QueryClient()}>
        <ConfigPanel />
      </QueryClientProvider>
    </MemoryRouter>
  )
}

/** The input drawn under a label (the panel puts them side by side, not in <label for>). */
function fieldUnder(label: string): HTMLInputElement {
  const input = screen.getByText(label, { selector: 'label' }).parentElement?.querySelector('input')
  if (!input) throw new Error(`no input under ${label}`)
  return input
}

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('every panel fallback is the run default', () => {
  it('split order: split size and total quantity', () => {
    const split = executorDefault('orders.rs', /integer\("splitSize", Some\((\d+)\)/)
    const quantity = executorDefault('orders.rs', /integer\("quantity", Some\((\d+)\)/)
    openPanel('splitOrder', { symbol: 'SBIN', exchange: 'NSE' })
    expect(fieldUnder('Split Size')).toHaveValue(Number(split))
    expect(fieldUnder('Total Quantity')).toHaveValue(Number(quantity))
    const orders = Math.ceil(Number(quantity) / Number(split))
    expect(screen.getByText(new RegExp(`Splits into\\s*${orders}\\s*orders`))).toBeInTheDocument()
  })

  it('smart order: position size', () => {
    const size = executorDefault('orders.rs', /integer\("positionSize", Some\((\d+)\)/)
    openPanel('smartOrder', { symbol: 'SBIN', exchange: 'NSE' })
    expect(fieldUnder('Position Size')).toHaveValue(Number(size))
  })

  it('history: interval, and no day range (decision 20)', () => {
    const interval = executorDefault('history.rs', /get_str\(data, "interval", "(\w+)"\)/)
    const days = executorDefault('history.rs', /get_int\(data, "days", (\d+)\)/)
    expect([interval, days]).toEqual(['5m', '0'])
    openPanel('history', { symbol: 'SBIN', exchange: 'NSE' })
    expect(screen.getByText('5 Min')).toBeInTheDocument()
    // 0 days asks for no range: the field stays empty and says it is required.
    expect(fieldUnder('Days')).toHaveValue(null)
    expect(screen.getByText(/Required: with no days the node stops/)).toBeInTheDocument()
  })

  it('indicator: interval, lookback, tail and bars back', () => {
    const interval = executorDefault('indicator.rs', /get_str\(data, "interval", "(\w+)"\)/)
    const lookback = executorDefault('indicator.rs', /get_int\(data, "lookbackBars", (\d+)\)/)
    const tail = executorDefault('indicator.rs', /get_int\(data, "tailBars", (\d+)\)/)
    const offset = executorDefault('indicator.rs', /get_int\(data, "offsetBars", (\d+)\)/)
    openPanel('indicator', { symbol: 'SBIN', exchange: 'NSE', indicatorName: 'rsi' })
    expect(fieldUnder('Interval')).toHaveValue(interval)
    expect(fieldUnder('Lookback Bars')).toHaveValue(Number(lookback))
    expect(fieldUnder('Tail Bars')).toHaveValue(Number(tail))
    expect(fieldUnder('Value N Bars Back')).toHaveValue(Number(offset))
  })

  it('bar offset: interval and bars back', () => {
    const source = rust('history.rs')
    const barOffset = source.slice(source.indexOf('"offsetBars"') - 400)
    const interval = barOffset.match(/get_str\(data, "interval", "(\w+)"\)/)?.[1]
    const offset = executorDefault('history.rs', /get_int\(data, "offsetBars", (\d+)\)/)
    openPanel('barOffset', { symbol: 'SBIN', exchange: 'NSE' })
    expect(fieldUnder('Interval')).toHaveValue(interval)
    expect(fieldUnder('Bars Back')).toHaveValue(Number(offset))
  })
})
