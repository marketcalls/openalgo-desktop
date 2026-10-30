/**
 * Journey 3: a workflow exported from OpenAlgo web, imported unchanged
 * through the Import dialog, run in analyzer mode, exported again, and the
 * export in the web's format (blueprints/flow.py export_workflow: name,
 * description, nodes, edges, version "1.0", exported_at) with no API key or
 * webhook credential in it (decisions.md 3, parity D-69), importing back.
 *
 * The workflow is example 8.6 of the web's docs/prompt/flow-import-format.md
 * (web commit 96e5927e5), copied byte for byte into
 * fixtures/web-8.6-nifty-custom-iron-fly.json: an iron fly on NIFTY's
 * current-week options, which the scripted market lists (ATM 25,000, the
 * OTM3 wings 25,150 and 24,850).
 */

import { readFileSync } from 'node:fs'
import { canvasEdges, canvasNodes, editorAction, executionLog, runNow } from './support/editor'
import { editorId, expect, test } from './support/fixtures'
import { LOT } from './support/http'

const WEB_EXPORT = readFileSync(
  new URL('./fixtures/web-8.6-nifty-custom-iron-fly.json', import.meta.url),
  'utf8'
)
const web = JSON.parse(WEB_EXPORT)

/** blueprints/flow.py export_workflow's keys. */
const WEB_EXPORT_KEYS = ['description', 'edges', 'exported_at', 'name', 'nodes', 'version']

test('3. import a web export, run it, export it in the web format', async ({ page, session }) => {
  const key = await session.apiKey()
  const before = new Set((await session.orderBook()).data.orders.map((o: any) => o.orderid))

  // The Import dialog, with the web's JSON pasted as it is.
  await page.goto('/flow')
  await page.getByRole('button', { name: 'Import', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Workflow JSON').fill(WEB_EXPORT)
  const imported = page.waitForResponse(
    (r) => r.request().method() === 'POST' && r.url().endsWith('/flow/api/workflows/import')
  )
  await dialog.getByRole('button', { name: 'Import', exact: true }).click()
  const answer = await imported
  expect(answer.status()).toBe(201)
  expect(await answer.json()).toMatchObject({ status: 'success' })
  await page.waitForURL(/\/flow\/editor\/\d+$/)
  const id = editorId(page)
  const storedName = `${web.name} (imported)`
  await expect(page.getByLabel('Workflow name')).toHaveValue(storedName)
  await expect(canvasNodes(page)).toHaveCount(web.nodes.length)
  await expect(canvasEdges(page)).toHaveCount(web.edges.length)

  // Stored as the web wrote it, inactive.
  const stored = await session.workflow(id)
  expect(stored.is_active).toBe(false)
  for (const node of web.nodes) {
    expect(stored.nodes.find((n: any) => n.id === node.id)).toMatchObject(node)
  }
  expect(stored.edges).toEqual(web.edges)

  // Run Now: four legs into the sandbox.
  const result = await runNow(page, id)
  expect(result.status).toBe('success')
  await expect(executionLog(page).getByText(`Starting workflow: ${storedName}`)).toBeVisible()
  const book = await session.orderBook()
  expect(book.mode).toBe('analyze')
  const legs = book.data.orders.filter((o: any) => !before.has(o.orderid))
  expect(legs).toHaveLength(4)
  for (const leg of legs) {
    expect(leg).toMatchObject({
      exchange: 'NFO',
      quantity: LOT,
      pricetype: 'MARKET',
      product: 'NRML',
      order_status: 'complete',
      strategy: storedName,
    })
    expect(leg.orderid).toMatch(/^\d{14}$/)
  }
  const sides = legs.map((l: any) => `${l.action} ${l.symbol.slice(-7)}`).sort()
  expect(sides).toEqual(['BUY 24850PE', 'BUY 25150CE', 'SELL 25000CE', 'SELL 25000PE'].sort())

  // Export Workflow from the editor's menu: the file a trader downloads.
  const download = page.waitForEvent('download')
  await editorAction(page, 'Export Workflow')
  const file = await download
  expect(file.suggestedFilename()).toBe('nifty_custom_iron_fly__imported_.json')
  const text = readFileSync((await file.path())!, 'utf8')
  const exported = JSON.parse(text)
  expect(Object.keys(exported).sort()).toEqual(WEB_EXPORT_KEYS)
  expect(exported.version).toBe('1.0')
  expect(exported.exported_at).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,6})?$/)
  expect(exported.name).toBe(storedName)
  expect(exported.description).toBe(web.description)
  for (const node of web.nodes) {
    expect(exported.nodes.find((n: any) => n.id === node.id)).toMatchObject(node)
  }
  expect(exported.edges).toEqual(web.edges)
  // No credential travels with a workflow.
  expect(text).not.toContain(key)
  for (const word of ['api_key', 'apikey', 'webhook_token', 'webhook_secret', 'secret']) {
    expect(text).not.toContain(word)
  }

  // The export imports again (the web's import rules, strict).
  const again = await session.post('/flow/api/workflows/import', exported)
  expect(again.status).toBe(201)
  expect(again.body.status).toBe('success')
  const copy = await session.workflow(again.body.workflow_id)
  expect(copy.name).toBe(`${storedName} (imported)`)
  expect(copy.edges).toEqual(web.edges)
})

test('3. the import route refuses a body that is not a workflow, as the web does', async ({
  session,
}) => {
  const count = (await session.workflows()).length
  // Nothing at all: the web's "No data provided" (R-14).
  const empty = await session.postRaw('/flow/api/workflows/import', '', 'application/json')
  expect(empty).toEqual({ status: 400, body: { error: 'No data provided' } })
  // Not JSON: a 400 in the route's own refusal shape, never a 500.
  const text = await session.postRaw('/flow/api/workflows/import', 'BUY NIFTY', 'application/json')
  expect(text.status).toBe(400)
  expect(text.body).toMatchObject({ status: 'error', error: 'Invalid workflow format' })
  // NaN, which Python's parser takes and the desktop refuses (decisions.md 15).
  const nan = await session.postRaw(
    '/flow/api/workflows/import',
    WEB_EXPORT.replace('"quantity": 1,', '"quantity": NaN,'),
    'application/json'
  )
  expect(nan.status).toBe(400)
  expect(nan.body).toMatchObject({ status: 'error', error: 'Invalid workflow format' })
  expect((await session.workflows()).length).toBe(count)
})
