/**
 * Journeys 1 and 2: a workflow built in the editor from the palette (a
 * schedule, a quote, a variable, a condition with both branches, an order and
 * two logs), wired, saved and reloaded unchanged; then Run Now in analyzer
 * mode: the run log in the editor, the order in the sandbox order book with
 * analyzer mode's markers, and nothing at the broker.
 */

import {
  canvasEdges,
  canvasNodes,
  closePanel,
  connect,
  createWorkflow,
  dropNode,
  executionLog,
  field,
  fill,
  nodeOfType,
  openNode,
  pick,
  picked,
  runNow,
  save,
  sourceHandle,
  targetHandle,
} from './support/editor'
import { expect, test, unique } from './support/fixtures'
import { LOT } from './support/http'

test.describe.configure({ mode: 'serial' })

const name = unique('E2E Build')
let workflowId = 0
let future = ''

/** Every edge as "sourceType -> targetType [handle]", sorted. */
function wiring(workflow: { nodes: any[]; edges: any[] }): string[] {
  const typeOf = new Map(workflow.nodes.map((n) => [n.id, n.type]))
  return workflow.edges
    .map(
      (e) =>
        `${typeOf.get(e.source)} -> ${typeOf.get(e.target)}${e.sourceHandle ? ` [${e.sourceHandle}]` : ''}`
    )
    .sort()
}

const EXPECTED_WIRING = [
  'getQuote -> variable',
  'placeOrder -> log',
  'start -> getQuote',
  'varCondition -> log [false]',
  'varCondition -> placeOrder [true]',
  'variable -> varCondition',
].sort()

test('1. build a workflow in the editor, save it, reload it unchanged', async ({
  page,
  session,
}) => {
  future = await session.niftyFuture()
  workflowId = await createWorkflow(page, name)

  // The palette, dragged onto the canvas, top to bottom.
  const start = await dropNode(page, 'Trigger', 'Schedule', 'start', { x: 300, y: 30 })
  const quote = await dropNode(page, 'Data', 'Get Quote', 'getQuote', { x: 300, y: 150 })
  const variable = await dropNode(page, 'Util', 'Variable', 'variable', { x: 300, y: 270 })
  const condition = await dropNode(page, 'Logic', 'Var Condition', 'varCondition', {
    x: 300,
    y: 390,
  })
  const order = await dropNode(page, 'Action', 'Place Order', 'placeOrder', { x: 120, y: 540 })
  const skipped = await dropNode(page, 'Util', 'Log', 'log', { x: 520, y: 540 })
  const placed = await dropNode(page, 'Util', 'Log', 'log', { x: 120, y: 690 })
  await expect(canvasNodes(page)).toHaveCount(7)

  // Each node's settings in its panel.
  let panel = await openNode(page, quote, 'Get Quote')
  await fill(panel, 'Symbol', 'NIFTY')
  await pick(page, panel, 'Exchange', 'NSE_INDEX')
  await fill(panel, 'Output Variable', 'quote')
  await closePanel(page)

  panel = await openNode(page, variable, 'Variable')
  await fill(panel, 'Variable Name', 'spot')
  await fill(panel, 'Value', '{{quote.data.ltp}}')
  await closePanel(page)

  panel = await openNode(page, condition, 'Var Condition')
  await fill(panel, 'Left Value', '{{spot}}')
  await fill(panel, 'Right Value', '25000')
  await closePanel(page)

  panel = await openNode(page, order, 'Place Order')
  await fill(panel, 'Symbol', future)
  await pick(page, panel, 'Exchange', 'NFO')
  await fill(panel, 'Quantity', String(LOT))
  await fill(panel, 'Output Variable', 'order')
  await closePanel(page)

  panel = await openNode(page, skipped, 'Log')
  await fill(panel, 'Message', 'Spot {{spot}} is not above 25000')
  await closePanel(page)

  panel = await openNode(page, placed, 'Log')
  await fill(panel, 'Message', 'Bought {{order.orderid}} at spot {{spot}}')
  await closePanel(page)

  // Wired handle to handle, the condition's two branches included.
  await connect(page, sourceHandle(start), targetHandle(quote))
  await connect(page, sourceHandle(quote), targetHandle(variable))
  await connect(page, sourceHandle(variable), targetHandle(condition))
  await connect(page, sourceHandle(condition, 'true'), targetHandle(order))
  await connect(page, sourceHandle(condition, 'false'), targetHandle(skipped))
  await connect(page, sourceHandle(order), targetHandle(placed))

  await save(page, workflowId)
  const stored = await session.workflow(workflowId)
  expect(stored.name).toBe(name)
  expect(stored.is_active).toBe(false)
  expect(stored.nodes.map((n: any) => n.type).sort()).toEqual(
    ['getQuote', 'log', 'log', 'placeOrder', 'start', 'varCondition', 'variable'].sort()
  )
  expect(wiring(stored)).toEqual(EXPECTED_WIRING)
  const data = (type: string) => stored.nodes.find((n: any) => n.type === type).data
  expect(data('getQuote')).toMatchObject({
    symbol: 'NIFTY',
    exchange: 'NSE_INDEX',
    outputVariable: 'quote',
  })
  expect(data('variable')).toMatchObject({
    variableName: 'spot',
    operation: 'set',
    value: '{{quote.data.ltp}}',
  })
  expect(data('varCondition')).toMatchObject({
    leftValue: '{{spot}}',
    operator: '>',
    rightValue: '25000',
  })
  expect(data('placeOrder')).toMatchObject({
    symbol: future,
    exchange: 'NFO',
    action: 'BUY',
    quantity: LOT,
    priceType: 'MARKET',
    outputVariable: 'order',
  })

  // Reloaded: the same graph on the canvas, the same settings in the panels,
  // and nothing left unsaved.
  await page.reload()
  await expect(canvasNodes(page)).toHaveCount(7)
  await expect(canvasEdges(page)).toHaveCount(6)
  await expect(page.getByLabel('Workflow name', { exact: true })).toHaveValue(name)
  await expect(page.getByText('Unsaved', { exact: true })).toHaveCount(0)
  panel = await openNode(page, nodeOfType(page, 'getQuote'), 'Get Quote')
  await expect(field(panel, 'Symbol')).toHaveValue('NIFTY')
  await expect(picked(panel, 'Exchange')).toHaveText('NSE_INDEX')
  await closePanel(page)
  panel = await openNode(page, nodeOfType(page, 'placeOrder'), 'Place Order')
  await expect(field(panel, 'Symbol')).toHaveValue(future)
  await expect(field(panel, 'Quantity')).toHaveValue(String(LOT))
  await closePanel(page)
  panel = await openNode(page, nodeOfType(page, 'log', 1), 'Log')
  await expect(field(panel, 'Message')).toHaveValue('Bought {{order.orderid}} at spot {{spot}}')
  await closePanel(page)
  const reloaded = await session.workflow(workflowId)
  expect(reloaded.nodes).toEqual(stored.nodes)
  expect(reloaded.edges).toEqual(stored.edges)
})

test('2. run it now in analyzer mode: the run log, the sandbox order book', async ({
  page,
  session,
}) => {
  expect(workflowId, 'journey 1 saved the workflow').toBeGreaterThan(0)
  const counted = await session.get('/e2e/scripted-broker')
  expect(counted.status).toBe(200)
  const before = counted.body

  await page.goto(`/flow/editor/${workflowId}`)
  await expect(canvasNodes(page)).toHaveCount(7)
  // Analyzer mode is on: the switch offers Live.
  await expect(page.getByRole('button', { name: 'Switch to Live mode' })).toBeVisible()

  const result = await runNow(page, workflowId)
  expect(result.status).toBe('success')

  // The run log in the editor, line by line as the executor wrote it.
  const log = executionLog(page)
  await expect(log).toBeVisible()
  await expect(log.getByText(`Starting workflow: ${name}`)).toBeVisible()
  await expect(log.getByText('Getting quote for: NIFTY')).toBeVisible()
  await expect(log.getByText(`Placing order: ${future} BUY qty=${LOT}`)).toBeVisible()
  await expect(log.getByText(/'mode': 'analyze'/).first()).toBeVisible()
  const bought = log.getByText(/^\[LOG\] Bought \d{14} at spot 25010(\.0)?$/)
  await expect(bought).toBeVisible()
  await expect(log.getByText(/is not above 25000/)).toHaveCount(0)
  const orderid = /Bought (\d{14})/.exec((await bought.textContent()) ?? '')![1]

  // The run's record.
  const runs = await session.executions(workflowId)
  expect(runs[0].status).toBe('completed')

  // The Order Book page: the sandbox order, filled, with its sandbox id.
  await page.goto('/orderbook')
  await expect(page.getByRole('button', { name: 'Switch to Live mode' })).toBeVisible()
  const row = page.locator('tbody tr').filter({ hasText: future })
  await expect(row).toHaveCount(1)
  for (const text of [orderid, 'NFO', 'BUY', String(LOT), 'MARKET', 'NRML', 'complete']) {
    await expect(row).toContainText(text)
  }

  // As an API client reads it: analyze mode, tagged with the workflow.
  const book = await session.orderBook()
  expect(book.mode).toBe('analyze')
  const mine = book.data.orders.filter((o: any) => o.orderid === orderid)
  expect(mine).toHaveLength(1)
  expect(mine[0]).toMatchObject({
    symbol: future,
    exchange: 'NFO',
    action: 'BUY',
    quantity: LOT,
    pricetype: 'MARKET',
    product: 'NRML',
    order_status: 'complete',
    strategy: name,
  })

  // The scripted broker quoted, and received no order.
  const after = (await session.get('/e2e/scripted-broker')).body
  expect(after.order_calls).toBe(before.order_calls)
  expect(after.calls).toBeGreaterThan(before.calls)
})
