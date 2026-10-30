/**
 * Journey 4: a workflow's webhook, enabled and read in its settings dialog,
 * called the way TradingView or Chartink would (no session, the URL and the
 * secret only): with the secret it runs and places a sandbox order; without
 * it, with a wrong one, or as plain text that cannot carry one, it is refused
 * and nothing runs. With the secret in the URL a plain-text alert runs, its
 * text in {{webhook.message}} (the web's _read_webhook_payload; decisions.md
 * 13: a secret is always required).
 */

import type { APIRequestContext, Locator, Page } from '@playwright/test'
import { expect, test, unique } from './support/fixtures'
import { LOT, type Session } from './support/http'

const MISSING_IN_PAYLOAD =
  "Missing webhook secret in payload. Send JSON with a 'secret' field. Plain text cannot carry one: switch the webhook to URL auth to authenticate with ?secret=... instead."

function webhookWorkflow(name: string) {
  return {
    name,
    description: 'Webhook to a sandbox order',
    nodes: [
      {
        id: 'node_1',
        type: 'webhookTrigger',
        position: { x: 100, y: 100 },
        data: { label: 'E2E alert' },
      },
      {
        id: 'node_2',
        type: 'log',
        position: { x: 100, y: 220 },
        data: { message: 'Alert for {{webhook.symbol}}: {{webhook.message}}', level: 'info' },
      },
      {
        id: 'node_3',
        type: 'placeOrder',
        position: { x: 100, y: 340 },
        data: {
          symbol: '{{webhook.symbol}}',
          exchange: 'NFO',
          action: 'BUY',
          quantity: LOT,
          priceType: 'MARKET',
          product: 'NRML',
          price: 0,
          triggerPrice: 0,
          outputVariable: 'order',
        },
      },
    ],
    edges: [
      { id: 'e1', source: 'node_1', target: 'node_2' },
      { id: 'e2', source: 'node_2', target: 'node_3' },
    ],
  }
}

/** The settings dialog of the workflow's card on the Flow list. */
async function openWebhookSettings(page: Page, name: string): Promise<Locator> {
  await page.goto('/flow')
  const card = page.getByRole('link', { name: `Open workflow ${name}` })
  await card.getByRole('button', { name: 'Workflow actions' }).click()
  await page.getByRole('menuitem', { name: 'Webhook' }).click()
  const dialog = page.getByRole('dialog').filter({ hasText: 'Webhook Settings' })
  await expect(dialog).toBeVisible()
  return dialog
}

function valueUnder(dialog: Locator, label: string): Locator {
  return dialog
    .locator('label')
    .filter({ hasText: new RegExp(`^${label.replace(/[()]/g, '\\$&')}$`) })
    .locator('xpath=..')
    .locator('input')
    .first()
}

async function runsOf(session: Session, id: number): Promise<number> {
  return (await session.executions(id)).length
}

test('4. a webhook with its secret runs; without it, refused', async ({
  page,
  session,
  playwright,
  baseURL,
}) => {
  const name = unique('E2E Webhook')
  const future = await session.niftyFuture()
  const created = await session.post('/flow/api/workflows/import', webhookWorkflow(name))
  expect(created.status).toBe(201)
  const id: number = created.body.workflow_id
  const storedName = `${name} (imported)`

  // Enable the webhook in its settings and read its address and secret.
  const dialog = await openWebhookSettings(page, storedName)
  const enabled = page.waitForResponse((r) =>
    r.url().endsWith(`/flow/api/workflows/${id}/webhook/enable`)
  )
  await dialog.getByRole('switch').click()
  expect((await enabled).status()).toBe(200)
  await expect(dialog.getByRole('switch')).toBeChecked()
  const url = await valueUnder(dialog, 'Webhook URL').inputValue()
  expect(url).toMatch(new RegExp(`^${baseURL}/flow/webhook/[A-Za-z0-9_-]+$`))
  await dialog.getByRole('button', { name: 'Show webhook secret' }).click()
  const secret = await valueUnder(dialog, 'Webhook Secret').inputValue()
  expect(secret.length).toBeGreaterThanOrEqual(16)
  await page.keyboard.press('Escape')

  // Activate it from its card.
  const card = page.getByRole('link', { name: `Open workflow ${storedName}` })
  await card.getByRole('button', { name: 'Workflow actions' }).click()
  const activated = page.waitForResponse((r) =>
    r.url().endsWith(`/flow/api/workflows/${id}/activate`)
  )
  await page.getByRole('menuitem', { name: 'Activate' }).click()
  expect((await activated).status()).toBe(200)

  // The sender: no session, no cookie, only what the trader pasted into it.
  const sender: APIRequestContext = await playwright.request.newContext()
  try {
    // Refused, and nothing runs: no secret, a wrong one, plain text.
    const runs = await runsOf(session, id)
    const none = await sender.post(url, { data: { symbol: future, action: 'BUY' } })
    expect(none.status()).toBe(401)
    expect(await none.json()).toEqual({ error: MISSING_IN_PAYLOAD })
    const wrong = await sender.post(url, { data: { secret: `${secret}x`, symbol: future } })
    expect(wrong.status()).toBe(401)
    expect(await wrong.json()).toEqual({ error: 'Invalid webhook secret' })
    const text = await sender.post(url, {
      headers: { 'Content-Type': 'text/plain' },
      data: `BUY ${future}`,
    })
    expect(text.status()).toBe(401)
    expect(await text.json()).toEqual({ error: MISSING_IN_PAYLOAD })
    const unknown = await sender.post(url.replace(/[^/]+$/, 'not-a-token'), { data: { secret } })
    expect(unknown.status()).toBe(404)
    expect(await runsOf(session, id)).toBe(runs)

    // With the secret: the run, its log and its sandbox order.
    const fired = await sender.post(url, { data: { secret, symbol: future, message: 'TV long' } })
    expect(fired.status()).toBe(200)
    const reply = await fired.json()
    expect(reply).toMatchObject({
      status: 'success',
      message: `Workflow '${storedName}' triggered`,
      workflow_id: id,
    })
    const run = (await session.executions(id))[0]
    expect(run.status).toBe('success')
    const logged = JSON.stringify(run.logs)
    expect(logged).toContain(`[LOG] Alert for ${future}: TV long`)
    expect(logged).toContain(`Placing order: ${future} BUY qty=${LOT}`)
    expect(logged).not.toContain(secret)
    const book = await session.orderBook()
    const orders = book.data.orders.filter((o: any) => o.strategy === storedName)
    expect(orders).toHaveLength(1)
    expect(orders[0]).toMatchObject({ symbol: future, order_status: 'complete' })

    // Secret in URL: a plain-text alert (Chartink) runs, its text the message.
    const settings = await openWebhookSettings(page, storedName)
    const switched = page.waitForResponse((r) =>
      r.url().endsWith(`/flow/api/workflows/${id}/webhook/auth-type`)
    )
    await settings.getByRole('button', { name: /Secret in URL/ }).click()
    expect((await switched).status()).toBe(200)
    await settings.getByRole('button', { name: 'Show webhook secret' }).click()
    const withSecret = await valueUnder(settings, 'Webhook URL (with secret)').inputValue()
    expect(withSecret).toBe(`${url}?secret=${secret}`)
    await page.keyboard.press('Escape')
    const before = await runsOf(session, id)
    const plain = await sender.post(`${url}/${future}?secret=${encodeURIComponent(secret)}`, {
      headers: { 'Content-Type': 'text/plain' },
      data: 'Chartink scan fired',
    })
    expect(plain.status()).toBe(200)
    expect(await plain.json()).toMatchObject({ status: 'success' })
    const runsAfter = await session.executions(id)
    expect(runsAfter).toHaveLength(before + 1)
    expect(JSON.stringify(runsAfter[0].logs)).toContain(
      `[LOG] Alert for ${future}: Chartink scan fired`
    )
    // And without it in the URL, refused.
    const bare = await sender.post(url, {
      headers: { 'Content-Type': 'text/plain' },
      data: 'fired',
    })
    expect(bare.status()).toBe(401)
    expect(await bare.json()).toEqual({
      error: 'Missing webhook secret in URL. Use ?secret=your_secret',
    })
    expect(await runsOf(session, id)).toBe(before + 1)
  } finally {
    await sender.dispose()
  }
})
