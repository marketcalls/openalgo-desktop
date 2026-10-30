/**
 * Journey 6: the Agent saves a workflow. A model on a local fake provider
 * (support/fake-provider.ts; no live model) answers the trader's request with
 * a save_flow call; the chat shows the approval card in a trader's words
 * (decisions.md 17); approved, the workflow is in the Flow list, inactive.
 */

import { startFakeProvider } from './support/fake-provider'
import { expect, test, unique } from './support/fixtures'

test('6. the Agent saves a workflow after the trader approves it', async ({ page, session }) => {
  const name = unique('E2E Agent')
  const workflow = {
    nodes: [
      {
        id: 'node_1',
        type: 'start',
        position: { x: 100, y: 100 },
        data: {
          scheduleType: 'daily',
          time: '09:20',
          days: [0, 1, 2, 3, 4],
          marketHoursOnly: true,
        },
      },
      {
        id: 'node_2',
        type: 'getQuote',
        position: { x: 100, y: 220 },
        data: { symbol: 'NIFTY', exchange: 'NSE_INDEX', outputVariable: 'quote' },
      },
      {
        id: 'node_3',
        type: 'log',
        position: { x: 100, y: 340 },
        data: { message: 'NIFTY at {{quote.data.ltp}}', level: 'info' },
      },
    ],
    edges: [
      { id: 'e1', source: 'node_1', target: 'node_2' },
      { id: 'e2', source: 'node_2', target: 'node_3' },
    ],
  }
  const provider = await startFakeProvider({ name, workflow_json: JSON.stringify(workflow) })
  try {
    // A model on the local provider, tested once, which makes it the default.
    const added = await session.post('/agent/api/models', {
      provider_kind: 'openai_compatible',
      model_name: 'gpt-fake',
      base_url: provider.endpoint,
      api_key: ['local', 'e2e', 'provider'].join('-'),
    })
    expect(added.status, JSON.stringify(added.body)).toBe(201)
    const tested = await session.post(`/agent/api/models/${added.body.data.id}/test`)
    expect(tested.body).toMatchObject({ ok: true })

    await page.goto('/agent')
    const composer = page.getByRole('textbox', { name: 'Message the agent' })
    await composer.fill(
      'Build a workflow that logs the NIFTY quote every weekday at 09:20, and save it.'
    )
    await page.getByRole('button', { name: 'Send the message' }).click()

    // The approval card, in a trader's words.
    const sentence = `Save the workflow "${name} (imported)" to Flow. It is saved inactive and does not run until you activate it.`
    await expect(page.getByText(sentence)).toBeVisible()
    await expect(page.getByText('3 nodes, 2 connections')).toBeVisible()
    expect(await session.workflows()).not.toContainEqual(
      expect.objectContaining({ name: `${name} (imported)` })
    )
    await page.getByRole('button', { name: 'Approve', exact: true }).click()
    await expect(
      page.getByText('Saved. It is inactive: open Flow to review and activate it.')
    ).toBeVisible()

    // In the Flow list, inactive, with the graph the Agent wrote.
    const saved = (await session.workflows()).find((w) => w.name === `${name} (imported)`)
    expect(saved, 'the saved workflow is listed').toBeTruthy()
    expect(saved.is_active).toBe(false)
    const stored = await session.workflow(saved.id)
    expect(stored.nodes.map((n: any) => n.type)).toEqual(['start', 'getQuote', 'log'])
    expect(stored.edges).toEqual(workflow.edges)
    await page.goto('/flow')
    const card = page.getByRole('link', { name: `Open workflow ${name} (imported)` })
    await expect(card).toBeVisible()
    await expect(card).toContainText('No executions')

    // The model was offered save_flow, and got the tool's answer back.
    const offeredSave = provider.requests.some((r) =>
      (r.body?.tools ?? []).some((t: any) => t?.function?.name === 'save_flow')
    )
    expect(offeredSave).toBe(true)
    const toolResult = provider.requests
      .flatMap((r) => r.body?.messages ?? [])
      .find((m: any) => m.role === 'tool')
    expect(JSON.stringify(toolResult)).toContain(`"workflow_id":${saved.id}`)
  } finally {
    await provider.close()
  }
})
