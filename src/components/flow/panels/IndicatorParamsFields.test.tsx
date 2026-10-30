/**
 * The Indicator node's parameter editor, pinned as the web renders it (parity
 * F-668, IND-084; the web has no test of it): one field per catalogue spec
 * by type, the number draft that commits only a finite value, unknown saved
 * keys kept after the spec keys, the JSON mode and its sentences, the
 * no-parameters state, and the fresh editor ConfigPanel mounts per node and
 * per indicator so a half-typed number cannot leak across them.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, fireEvent, render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { MemoryRouter } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/flow')>()
  return {
    ...actual,
    getIndexSymbolsLotSizes: vi.fn(async () => []),
    getWebhookInfo: vi.fn(async () => null),
  }
})

/**
 * The catalogue has no free-text parameter today (every string one has
 * choices), so a probe indicator exercises the component's plain-text branch.
 */
vi.mock('@/lib/flow/constants', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/lib/flow/constants')>()
  return {
    ...actual,
    INDICATOR_PARAMS: {
      ...actual.INDICATOR_PARAMS,
      probe_text: [{ name: 'column', label: 'Column', type: 'string', default: 'close' }],
    },
  }
})

const { IndicatorParamsFields } = await import('./IndicatorParamsFields')
const { ConfigPanel } = await import('./ConfigPanel')

function renderFields(indicatorName: string, value = '') {
  const onChange = vi.fn()
  render(<IndicatorParamsFields indicatorName={indicatorName} value={value} onChange={onChange} />)
  return { onChange }
}

/** Feeds onChange back into value, the way ConfigPanel does. */
function Controlled({ indicatorName, initial }: { indicatorName: string; initial: string }) {
  const [value, setValue] = useState(initial)
  return <IndicatorParamsFields indicatorName={indicatorName} value={value} onChange={setValue} />
}

/** The control under a field's label (the labels are not tied to their inputs). */
function field(label: string, within_: HTMLElement = document.body): HTMLElement {
  const holder = within(within_).getByText(label, { selector: 'label' }).parentElement
  const control = holder?.querySelector<HTMLElement>('input, button')
  if (!control) throw new Error(`no control for ${label}`)
  return control
}

const input = (label: string) => field(label) as HTMLInputElement

const JSON_HELP =
  'Raw kwargs passed to the indicator. Use this for {{variable}} references, which are interpolated before the JSON is parsed - the fields view cannot represent them.'

function parseError(raw: string): string {
  try {
    JSON.parse(raw)
  } catch (e) {
    return (e as Error).message
  }
  throw new Error(`${raw} parses`)
}

describe('F-668 one field per catalogue spec', () => {
  it('lists the spec labels in catalogue order under "Parameters"', () => {
    renderFields('alma')
    expect(screen.getByText('Parameters')).toBeInTheDocument()
    expect(screen.getAllByText(/.+/, { selector: 'label' }).map((l) => l.textContent)).toEqual([
      'Parameters',
      'Period',
      'Offset',
      'Sigma',
    ])
  })

  it('draws a whole-number box for an int, showing and suggesting the default', () => {
    renderFields('alma')
    const period = input('Period')
    expect(period).toHaveAttribute('type', 'number')
    expect(period).toHaveAttribute('step', '1')
    expect(period).toHaveAttribute('placeholder', '21')
    expect(period.value).toBe('21')
  })

  it('draws a free decimal box for a float', () => {
    renderFields('alma')
    expect(input('Offset')).toHaveAttribute('step', 'any')
    expect(input('Offset')).toHaveAttribute('placeholder', '0.85')
    expect(input('Offset').value).toBe('0.85')
    expect(input('Sigma')).toHaveAttribute('placeholder', '6')
  })

  it('shows a saved value over the default', () => {
    renderFields('alma', '{"period": 9, "sigma": 4.5}')
    expect(input('Period').value).toBe('9')
    expect(input('Sigma').value).toBe('4.5')
    expect(input('Offset').value).toBe('0.85')
  })

  it('draws a switch for a bool and writes true or false', async () => {
    const { onChange } = renderFields('dpo')
    const centred = field('Is Centered')
    expect(centred).toHaveAttribute('role', 'switch')
    expect(centred).toHaveAttribute('aria-checked', 'false')
    await userEvent.click(centred)
    expect(onChange).toHaveBeenLastCalledWith('{"is_centered":true}')
  })

  it('shows a saved bool on its switch', () => {
    renderFields('dpo', '{"is_centered": true}')
    expect(field('Is Centered')).toHaveAttribute('aria-checked', 'true')
  })

  it('draws a select for a spec with choices and writes the choice', async () => {
    const { onChange } = renderFields('apo')
    const maType = field('MA Type')
    expect(maType).toHaveAttribute('role', 'combobox')
    expect(maType.textContent).toBe('SMA')
    await userEvent.click(maType)
    const options = await screen.findAllByRole('option')
    expect(options.map((o) => o.textContent)).toEqual(['SMA', 'EMA'])
    await userEvent.click(screen.getByRole('option', { name: 'EMA' }))
    expect(onChange).toHaveBeenLastCalledWith('{"ma_type":"EMA"}')
  })

  it('draws a text box for a string with no choices and writes the text', () => {
    const { onChange } = renderFields('probe_text')
    const column = input('Column')
    expect(column).not.toHaveAttribute('type', 'number')
    expect(column).toHaveAttribute('placeholder', 'close')
    expect(column.value).toBe('close')
    fireEvent.change(column, { target: { value: 'hl2' } })
    expect(onChange).toHaveBeenLastCalledWith('{"column":"hl2"}')
  })

  it.each(['adl', 'not_an_indicator'])('says %s takes no parameters', (name) => {
    renderFields(name)
    expect(screen.getByText('This indicator takes no parameters.')).toBeInTheDocument()
    expect(screen.queryByRole('spinbutton')).toBeNull()
  })
})

describe('IND-084 number drafts and what an edit writes', () => {
  it('keeps a cleared box empty and commits nothing', () => {
    const { onChange } = renderFields('alma')
    fireEvent.change(input('Period'), { target: { value: '' } })
    expect(input('Period').value).toBe('')
    expect(onChange).not.toHaveBeenCalled()
  })

  it('commits an int parsed whole and a float as typed', () => {
    const { onChange } = renderFields('alma')
    fireEvent.change(input('Period'), { target: { value: '12.7' } })
    expect(onChange).toHaveBeenLastCalledWith('{"period":12}')
    fireEvent.change(input('Offset'), { target: { value: '0.9' } })
    expect(onChange).toHaveBeenLastCalledWith('{"offset":0.9}')
  })

  it('shows the typed text until the box is left, then the stored value', () => {
    renderFields('alma', '{"period": 9}')
    fireEvent.change(input('Period'), { target: { value: '30' } })
    expect(input('Period').value).toBe('30')
    fireEvent.blur(input('Period'))
    expect(input('Period').value).toBe('9')
  })

  it('keeps the committed value once the node holds it', () => {
    render(<Controlled indicatorName="alma" initial="" />)
    fireEvent.change(input('Period'), { target: { value: '' } })
    fireEvent.change(input('Period'), { target: { value: '30' } })
    fireEvent.blur(input('Period'))
    expect(input('Period').value).toBe('30')
  })

  it('keeps a draft to its own field', () => {
    renderFields('alma')
    fireEvent.change(input('Period'), { target: { value: '' } })
    expect(input('Offset').value).toBe('0.85')
  })

  it('writes the spec keys in catalogue order, then the unknown saved keys', () => {
    const { onChange } = renderFields('alma', '{"custom": 1, "sigma": 5, "period": 21}')
    fireEvent.change(input('Offset'), { target: { value: '0.9' } })
    expect(onChange).toHaveBeenLastCalledWith('{"period":21,"offset":0.9,"sigma":5,"custom":1}')
  })

  it('reads an empty value as all defaults, with no error', () => {
    renderFields('alma', '   ')
    expect(input('Period').value).toBe('21')
    expect(screen.getByRole('button', { name: 'Edit as JSON' })).toBeInTheDocument()
  })
})

describe('F-668 JSON mode', () => {
  it('switches to a JSON box with its example and help, and back', async () => {
    renderFields('alma', '{"period": 9}')
    await userEvent.click(screen.getByRole('button', { name: 'Edit as JSON' }))

    const box = screen.getByPlaceholderText('{"period": 14}') as HTMLInputElement
    expect(box.value).toBe('{"period": 9}')
    expect(screen.getByText(JSON_HELP)).toHaveClass('text-muted-foreground')
    expect(screen.queryByText('Period', { selector: 'label' })).toBeNull()

    await userEvent.click(screen.getByRole('button', { name: 'Use fields' }))
    expect(input('Period').value).toBe('9')
  })

  it('writes the JSON text as typed, an empty box as an empty string', async () => {
    const { onChange } = renderFields('alma', '{"period": 9}')
    await userEvent.click(screen.getByRole('button', { name: 'Edit as JSON' }))
    const box = screen.getByPlaceholderText('{"period": 14}')
    fireEvent.change(box, { target: { value: '{"period": {{len}}}' } })
    expect(onChange).toHaveBeenLastCalledWith('{"period": {{len}}}')
    fireEvent.change(box, { target: { value: '' } })
    expect(onChange).toHaveBeenLastCalledWith('')
  })

  it('holds a {{variable}} params text in JSON mode, showing the parser message', async () => {
    const raw = '{"period": {{len}}}'
    renderFields('alma', raw)
    expect((screen.getByPlaceholderText('{"period": 14}') as HTMLInputElement).value).toBe(raw)
    expect(screen.getByText(parseError(raw))).toHaveClass('text-destructive')
    expect(screen.queryByText(JSON_HELP)).toBeNull()

    await userEvent.click(screen.getByRole('button', { name: 'Use fields' }))
    expect(screen.getByPlaceholderText('{"period": 14}')).toBeInTheDocument()
    expect(screen.queryByText('Period', { selector: 'label' })).toBeNull()
  })

  it.each(['[14]', '14', 'null', '"period"'])('refuses %s as not a JSON object', (raw) => {
    renderFields('alma', raw)
    expect(screen.getByText('Params must be a JSON object, e.g. {"period": 14}')).toHaveClass(
      'text-destructive'
    )
  })

  describe('when the parser throws something that is not an Error', () => {
    const parse = JSON.parse
    beforeEach(() => {
      vi.spyOn(JSON, 'parse').mockImplementation((...args: Parameters<typeof JSON.parse>) => {
        if (args[0] === '{oops') throw 'not an Error'
        return parse(...args)
      })
    })
    afterEach(() => vi.restoreAllMocks())

    it('says Invalid JSON', () => {
      renderFields('alma', '{oops')
      expect(screen.getByText('Invalid JSON')).toHaveClass('text-destructive')
    })
  })
})

describe('IND-084 a fresh editor per node and per indicator', () => {
  beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

  function renderPanel() {
    const node = (id: string, indicatorName: string, params: string) => ({
      id,
      type: 'indicator',
      position: { x: 0, y: 0 },
      data: { indicatorName, symbol: 'SBIN', exchange: 'NSE', interval: 'D', params },
    })
    useFlowWorkflowStore.setState({
      nodes: [node('a', 'rsi', '{"period": 9}'), node('b', 'rsi', '{"period": 21}')],
      selectedNodeId: 'a',
    })
    const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } })
    render(
      <MemoryRouter>
        <QueryClientProvider client={client}>
          <ConfigPanel />
        </QueryClientProvider>
      </MemoryRouter>
    )
  }

  /** The Period box inside the Parameters editor. */
  const period = () => {
    const editor = screen.getByText('Parameters', { selector: 'label' }).closest('div.space-y-3')
    return field('Period', editor as HTMLElement) as HTMLInputElement
  }

  it('does not carry a half-typed number to the next indicator node', () => {
    renderPanel()
    expect(period().value).toBe('9')
    fireEvent.change(period(), { target: { value: '' } })
    expect(period().value).toBe('')

    act(() => useFlowWorkflowStore.getState().selectNode('b'))
    expect(period().value).toBe('21')
  })

  it('does not carry a half-typed number to another indicator on the same node', () => {
    renderPanel()
    fireEvent.change(period(), { target: { value: '' } })

    act(() => useFlowWorkflowStore.getState().updateNodeData('a', { indicatorName: 'ema' }))
    expect(period().value).toBe('9')
  })
})
