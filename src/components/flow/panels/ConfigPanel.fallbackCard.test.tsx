/**
 * What the config panel draws for a node type it has no form for, and the
 * Node ID footer under every form. Pins parity F-619 and F-620.
 *
 * Every type the palette offers has a form today, so the card that shows a
 * palette entry's label and description is reached only through a palette
 * entry the panel does not know: this file adds one to NODE_DEFINITIONS.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
}))

vi.mock('@/lib/flow/constants', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/lib/flow/constants')>()
  const paletteOnly = {
    type: 'paletteOnly',
    label: 'Palette Only',
    description: 'A palette entry the panel has no form for',
    category: 'utility' as const,
  }
  return {
    ...actual,
    NODE_DEFINITIONS: {
      ...actual.NODE_DEFINITIONS,
      UTILITIES: [...actual.NODE_DEFINITIONS.UTILITIES, paletteOnly],
    },
  }
})

const { ConfigPanel } = await import('./ConfigPanel')

function openPanel(type: string, data: Record<string, unknown> = {}, id = 'n1') {
  useFlowWorkflowStore.setState({
    nodes: [{ id, type, position: { x: 0, y: 0 }, data }],
    selectedNodeId: id,
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
const title = () => screen.getByRole('heading', { level: 2 }).textContent

function nodeLabelInput(): HTMLInputElement {
  const input = screen
    .getByText('Node Label', { selector: 'label' })
    .parentElement?.querySelector('input')
  if (!input) throw new Error('no Node Label input')
  return input
}

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('a node type with no form (F-619)', () => {
  it('shows its palette label and description, then a Node Label box', () => {
    const { container } = openPanel('paletteOnly')
    expect(title()).toBe('Palette Only')
    const card = screen.getByText('Palette Only', { selector: 'p' }).closest('.rounded-lg')
    expect(card).not.toBeNull()
    expect(card).toHaveTextContent('A palette entry the panel has no form for')
    expect(card?.querySelector('svg')).not.toBeNull()
    // Card, separator, Node Label; then the footer's own separator.
    expect(container.querySelectorAll('[data-slot="separator"]')).toHaveLength(2)
    const box = nodeLabelInput()
    expect(box.parentElement?.previousElementSibling).toHaveAttribute('data-slot', 'separator')
    expect(box).toHaveAttribute('placeholder', 'Enter label...')
  })

  it('stores the label typed', () => {
    openPanel('paletteOnly', { label: 'Mine' })
    const input = nodeLabelInput()
    expect(input).toHaveValue('Mine')
    fireEvent.change(input, { target: { value: 'Renamed' } })
    expect(stored().label).toBe('Renamed')
  })

  it('shows only the Node Label box for a type the palette does not know either', () => {
    openPanel('fromAnotherVersion')
    expect(title()).toBe('fromAnotherVersion')
    expect(screen.queryByText('Palette Only')).not.toBeInTheDocument()
    expect(nodeLabelInput()).toHaveValue('')
  })

  it('does not draw the card or the Node Label box for a type with a form', () => {
    openPanel('delay')
    expect(screen.queryByText('Node Label', { selector: 'label' })).not.toBeInTheDocument()
  })
})

describe('the Node ID footer (F-620)', () => {
  it.each([
    ['delay', 'node-delay-7'],
    ['notGate', 'gate_42'],
    ['paletteOnly', 'p1'],
    ['fromAnotherVersion', 'x-1'],
  ])('a %s node shows its id %s in code under Node ID', (type, id) => {
    openPanel(type, {}, id)
    const label = screen.getByText('Node ID', { selector: 'label' })
    const code = label.parentElement?.querySelector('code')
    expect(code).toHaveTextContent(id)
    expect(code).toHaveClass('font-mono')
    expect(label.parentElement?.previousElementSibling).toHaveAttribute('data-slot', 'separator')
  })
})

describe('F-619: the card shows the info icon', () => {
  it('draws the Info icon beside the palette label', () => {
    openPanel('paletteOnly')
    const card = screen.getByText('Palette Only', { selector: 'p' }).closest('.rounded-lg')
    expect(card?.querySelector('svg')).toHaveClass('lucide-info')
  })
})
