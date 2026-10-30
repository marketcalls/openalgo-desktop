/**
 * The utility nodes' config forms, as the web draws them: delay, wait until,
 * log, the Telegram and WhatsApp alerts, variable, math expression and HTTP
 * request. Labels, placeholders, help sentences, Select choices, the fields
 * each choice shows, and what an edit stores, with the number fallbacks.
 * Pins parity F-600 to F-607.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { useFlowWorkflowStore } from '@/stores/flowWorkflowStore'

vi.mock('@/api/flow', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/flow')>()),
  getIndexSymbolsLotSizes: vi.fn(async () => []),
}))

const { ConfigPanel } = await import('./ConfigPanel')

function openPanel(type: string, data: Record<string, unknown> = {}) {
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

/** The control drawn beside a label (the panel puts them side by side, not in <label for>). */
function under<K extends 'input' | 'textarea' | 'button'>(
  label: string,
  tag: K
): HTMLElementTagNameMap[K] {
  const el = screen.getByText(label, { selector: 'label' }).parentElement?.querySelector(tag)
  if (!el) throw new Error(`no ${tag} under ${label}`)
  return el
}

const shownUnder = (label: string) => under(label, 'button').textContent
const labelled = (label: string) => screen.queryByText(label, { selector: 'label' })

/** Opens the Select under `label` and returns its options as drawn. */
function openSelect(label: string): HTMLElement[] {
  fireEvent.click(under(label, 'button'))
  return Array.from(screen.getByRole('listbox').querySelectorAll<HTMLElement>('[role="option"]'))
}

const texts = (options: HTMLElement[]) => options.map((o) => o.textContent)

function pick(option: string) {
  fireEvent.click(within(screen.getByRole('listbox')).getByText(option))
}

beforeEach(() => useFlowWorkflowStore.getState().resetWorkflow())

describe('delay (F-600)', () => {
  it('waits 1 second when nothing is stored', () => {
    openPanel('delay')
    const amount = under('Wait Duration', 'input')
    expect(amount).toHaveAttribute('type', 'number')
    expect(amount).toHaveAttribute('min', '1')
    expect(amount).toHaveValue(1)
    expect(shownUnder('Wait Duration')).toBe('Seconds')
  })

  it('stores a whole duration, and 1 for blank', () => {
    openPanel('delay', { delayValue: 5, delayUnit: 'seconds' })
    const amount = under('Wait Duration', 'input')
    fireEvent.change(amount, { target: { value: '15' } })
    expect(stored().delayValue).toBe(15)
    fireEvent.change(amount, { target: { value: '' } })
    expect(stored().delayValue).toBe(1)
    expect(amount).toHaveValue(1)
  })

  it('offers seconds, minutes and hours and stores the unit', () => {
    openPanel('delay', { delayValue: 5 })
    expect(texts(openSelect('Wait Duration'))).toEqual(['Seconds', 'Minutes', 'Hours'])
    pick('Minutes')
    expect(stored().delayUnit).toBe('minutes')
  })
})

describe('waitUntil (F-601)', () => {
  it('pauses until 09:30 when no time is stored and stores the time set', () => {
    openPanel('waitUntil')
    const time = under('Target Time', 'input')
    expect(time).toHaveAttribute('type', 'time')
    expect(time).toHaveValue('09:30')
    expect(screen.getByText('Workflow pauses until this time')).toBeInTheDocument()
    fireEvent.change(time, { target: { value: '14:45' } })
    expect(stored().targetTime).toBe('14:45')
  })

  it('takes a label', () => {
    openPanel('waitUntil')
    const label = under('Label', 'input')
    expect(label).toHaveAttribute('placeholder', 'Wait for Entry')
    fireEvent.change(label, { target: { value: 'Open' } })
    expect(stored().label).toBe('Open')
  })
})

describe('log (F-602)', () => {
  it('logs at info by default and offers three coloured levels', () => {
    openPanel('log')
    expect(shownUnder('Log Level')).toBe('Info')
    const options = openSelect('Log Level')
    expect(texts(options)).toEqual(['Info', 'Warning', 'Error'])
    const colour = (o: HTMLElement) => within(o).getByText(o.textContent ?? '').className
    expect(options.map(colour)).toEqual(['text-blue-400', 'text-yellow-400', 'text-red-400'])
    pick('Warning')
    expect(stored().level).toBe('warn')
  })

  it('takes a message with variables', () => {
    openPanel('log')
    const message = under('Message', 'textarea')
    expect(message).toHaveAttribute('placeholder', 'Log message with {{variables}}')
    fireEvent.change(message, { target: { value: 'LTP {{quote.ltp}}' } })
    expect(stored().message).toBe('LTP {{quote.ltp}}')
  })
})

describe('telegramAlert (F-603)', () => {
  it('takes a message and says which account receives it', () => {
    openPanel('telegramAlert')
    const message = under('Message', 'textarea')
    expect(message).toHaveAttribute('placeholder', 'Order placed for {{orderResult.symbol}}')
    fireEvent.change(message, { target: { value: 'Filled' } })
    expect(stored().message).toBe('Filled')
    expect(
      screen.getByText("Telegram delivery uses the account linked to the workflow owner's API key.")
    ).toBeInTheDocument()
  })
})

describe('whatsappAlert (F-604)', () => {
  it('sends to yourself unless a number is given', () => {
    openPanel('whatsappAlert')
    const to = under('To (optional)', 'input')
    expect(to).toHaveValue('')
    expect(to).toHaveAttribute('placeholder', '919876543210 - blank sends to yourself')
    fireEvent.change(to, { target: { value: '919800000000' } })
    expect(stored().to).toBe('919800000000')
  })

  it('takes a message, lists the variables and says the bot must be paired', () => {
    openPanel('whatsappAlert')
    const message = under('Message', 'textarea')
    expect(message).toHaveAttribute('placeholder', 'Order placed for {{orderResult.symbol}}')
    fireEvent.change(message, { target: { value: 'Filled' } })
    expect(stored().message).toBe('Filled')
    expect(screen.getByText('Variables:')).toBeInTheDocument()
    expect(
      screen.getByText('{{orderResult.orderid}}, {{quote.ltp}}, {{timestamp}}')
    ).toBeInTheDocument()
    expect(
      screen.getByText('Requires the WhatsApp bot to be paired from the /whatsapp page first.')
    ).toBeInTheDocument()
  })
})

describe('variable (F-605)', () => {
  it('names the variable and shows how to read it', () => {
    openPanel('variable')
    const name = under('Variable Name', 'input')
    expect(name).toHaveAttribute('placeholder', 'myLTP')
    expect(screen.getByText('Use: {{varName}}')).toBeInTheDocument()
    fireEvent.change(name, { target: { value: 'qty' } })
    expect(stored().variableName).toBe('qty')
    expect(screen.getByText('Use: {{qty}}')).toBeInTheDocument()
  })

  it('offers eleven operations, Set Value by default, and stores the pick', () => {
    openPanel('variable', { variableName: 'qty' })
    expect(shownUnder('Operation')).toBe('Set Value')
    expect(texts(openSelect('Operation'))).toEqual([
      'Set Value',
      'Get Value',
      'Add',
      'Subtract',
      'Multiply',
      'Divide',
      'Increment',
      'Decrement',
      'Parse JSON',
      'Stringify',
      'Append',
    ])
    pick('Parse JSON')
    expect(stored().operation).toBe('parse_json')
  })

  it.each(['set', 'add', 'subtract', 'multiply', 'divide', 'append'])(
    '%s takes a value or a {{variable}}, and nothing else',
    (operation) => {
      openPanel('variable', { variableName: 'qty', operation })
      const value = under('Value', 'input')
      expect(value).toHaveAttribute('placeholder', 'Value or {{variable}}')
      fireEvent.change(value, { target: { value: '{{ltp}}' } })
      expect(stored().value).toBe('{{ltp}}')
      expect(labelled('Source Variable')).not.toBeInTheDocument()
      expect(labelled('JSON Path')).not.toBeInTheDocument()
    }
  )

  it('parse_json takes a JSON string in a code box', () => {
    openPanel('variable', { variableName: 'cfg', operation: 'parse_json' })
    expect(labelled('Value')).not.toBeInTheDocument()
    const json = under('JSON String', 'textarea')
    expect(json).toHaveClass('font-mono')
    expect(json).toHaveAttribute('placeholder', '{"key": "value"}')
    fireEvent.change(json, { target: { value: '{"a": 1}' } })
    expect(stored().value).toBe('{"a": 1}')
  })

  it('get reads a path from a source variable', () => {
    openPanel('variable', { variableName: 'ltp', operation: 'get' })
    expect(labelled('Value')).not.toBeInTheDocument()
    const source = under('Source Variable', 'input')
    expect(source).toHaveAttribute('placeholder', 'quoteData')
    fireEvent.change(source, { target: { value: 'quote' } })
    expect(stored().sourceVariable).toBe('quote')
    const path = under('JSON Path', 'input')
    expect(path).toHaveAttribute('placeholder', 'data.ltp')
    fireEvent.change(path, { target: { value: 'data.ltp' } })
    expect(stored().jsonPath).toBe('data.ltp')
  })

  it('stringify reads a source variable and no path', () => {
    openPanel('variable', { variableName: 's', operation: 'stringify' })
    expect(under('Source Variable', 'input')).toHaveAttribute('placeholder', 'quoteData')
    expect(labelled('JSON Path')).not.toBeInTheDocument()
    expect(labelled('Value')).not.toBeInTheDocument()
  })

  it.each(['increment', 'decrement'])('%s shows no field beyond the operation', (operation) => {
    openPanel('variable', { variableName: 'n', operation })
    for (const label of ['Value', 'JSON String', 'Source Variable', 'JSON Path']) {
      expect(labelled(label), label).not.toBeInTheDocument()
    }
  })
})

describe('mathExpression (F-606)', () => {
  it('takes an expression in a code box and says which operators it supports', () => {
    openPanel('mathExpression')
    const expression = under('Expression', 'textarea')
    expect(expression).toHaveClass('font-mono')
    expect(expression).toHaveAttribute('placeholder', '({{ltp}} * {{lotSize}}) + {{brokerage}}')
    expect(screen.getByText('Supports: +, -, *, /, %, ** (power)')).toBeInTheDocument()
    fireEvent.change(expression, { target: { value: '{{a}} + 1' } })
    expect(stored().expression).toBe('{{a}} + 1')
  })
})

describe('httpRequest (F-607)', () => {
  const METHODS = ['GET', 'POST', 'PUT', 'DEL', 'PATCH']
  const method = (name: string) => screen.getByRole('button', { name })

  it('lights no method until one is stored, and stores DEL as DELETE', () => {
    openPanel('httpRequest')
    for (const name of METHODS) expect(method(name)).not.toHaveClass('bg-primary')
    fireEvent.click(method('DEL'))
    expect(stored().method).toBe('DELETE')
    expect(method('DEL')).toHaveClass('bg-primary')
    expect(method('GET')).not.toHaveClass('bg-primary')
  })

  it.each([
    ['GET', 'GET', false],
    ['DELETE', 'DEL', false],
    ['POST', 'POST', true],
    ['PUT', 'PUT', true],
    ['PATCH', 'PATCH', true],
  ])('a %s request (button %s) shows a body: %s', (storedMethod, button, hasBody) => {
    openPanel('httpRequest', { method: storedMethod })
    expect(method(button)).toHaveClass('bg-primary')
    expect(labelled('Body (JSON)') !== null).toBe(hasBody)
  })

  it('treats a node with no method as GET: no body', () => {
    openPanel('httpRequest')
    expect(labelled('Body (JSON)')).not.toBeInTheDocument()
  })

  it('takes a URL, JSON headers and a JSON body', () => {
    openPanel('httpRequest', { method: 'POST' })
    const url = under('URL', 'input')
    expect(url).toHaveAttribute('placeholder', 'https://api.example.com')
    fireEvent.change(url, { target: { value: 'https://x.test/hook' } })
    expect(stored().url).toBe('https://x.test/hook')

    const headers = under('Headers (JSON)', 'textarea')
    expect(headers).toHaveClass('font-mono')
    expect(headers).toHaveAttribute('placeholder', '{"Authorization": "Bearer {{token}}"}')
    fireEvent.change(headers, { target: { value: '{"A": "b"}' } })
    expect(stored().headers).toBe('{"A": "b"}')

    const body = under('Body (JSON)', 'textarea')
    expect(body).toHaveClass('font-mono')
    expect(body).toHaveAttribute('placeholder', '{"symbol": "{{webhook.symbol}}"}')
    fireEvent.change(body, { target: { value: '{"q": 1}' } })
    expect(stored().body).toBe('{"q": 1}')
  })

  it('times out at 30000 ms by default, 1000 to 60000, and 30000 again for blank', () => {
    openPanel('httpRequest', { method: 'GET' })
    const timeout = under('Timeout (ms)', 'input')
    expect(timeout).toHaveValue(30000)
    expect(timeout).toHaveAttribute('min', '1000')
    expect(timeout).toHaveAttribute('max', '60000')
    fireEvent.change(timeout, { target: { value: '5000' } })
    expect(stored().timeout).toBe(5000)
    fireEvent.change(timeout, { target: { value: '' } })
    expect(stored().timeout).toBe(30000)
  })

  it('shows apiResponse as the output hint and says how to read it', () => {
    openPanel('httpRequest')
    const output = under('Output Variable', 'input')
    expect(output).toHaveValue('')
    expect(output).toHaveAttribute('placeholder', 'apiResponse')
    expect(screen.getByText('Use {{apiResponse.data}}')).toBeInTheDocument()
    fireEvent.change(output, { target: { value: 'resp' } })
    expect(stored().outputVariable).toBe('resp')
  })
})

describe('F-600: the delay unit and a fractional duration', () => {
  it.each([
    ['Hours', 'hours'],
    ['Seconds', 'seconds'],
  ])('stores %s as %s', (label, value) => {
    openPanel('delay', { delayValue: 2, delayUnit: 'minutes' })
    openSelect('Wait Duration')
    pick(label)
    expect(stored().delayUnit).toBe(value)
  })

  it.each([
    ['seconds', 'Seconds'],
    ['minutes', 'Minutes'],
    ['hours', 'Hours'],
  ])('a stored %s shows %s', (delayUnit, shown) => {
    openPanel('delay', { delayValue: 2, delayUnit })
    expect(shownUnder('Wait Duration')).toBe(shown)
  })

  it.each([
    ['2.7', 2],
    ['0.5', 1],
  ])('typing %j stores the whole number %s', (typed, value) => {
    openPanel('delay', { delayValue: 5, delayUnit: 'seconds' })
    fireEvent.change(under('Wait Duration', 'input'), { target: { value: typed } })
    expect(stored().delayValue).toBe(value)
  })
})

describe('F-602: the Error log level', () => {
  it('stores Error as error', () => {
    openPanel('log', { level: 'info' })
    openSelect('Log Level')
    pick('Error')
    expect(stored().level).toBe('error')
  })

  it.each([
    ['warn', 'Warning'],
    ['error', 'Error'],
  ])('a stored %s shows %s', (level, shown) => {
    openPanel('log', { level })
    expect(shownUnder('Log Level')).toBe(shown)
  })
})

describe('F-607: a fractional HTTP timeout', () => {
  it.each([
    ['2500.9', 2500],
    ['0.5', 30000],
  ])('typing %j stores %s', (typed, value) => {
    openPanel('httpRequest', { method: 'GET', timeout: 10000 })
    fireEvent.change(under('Timeout (ms)', 'input'), { target: { value: typed } })
    expect(stored().timeout).toBe(value)
  })
})
