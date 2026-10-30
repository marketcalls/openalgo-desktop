/**
 * Journey 5: a schedule built in the editor (every 2 seconds, outside the
 * market-hours gate so the journey runs at any hour), activated, firing on
 * the app's own clock; its runs in the history and on the Flow list; then
 * deactivated, and seen to stop.
 *
 * Proving that nothing more runs needs a window of time. Instead of a fixed
 * sleep, a second scheduled workflow (the witness, every second) is watched
 * for fresh runs: once the witness has fired twice more, the scheduler has
 * demonstrably been ticking while the deactivated workflow stayed silent.
 */

import type { Page } from '@playwright/test'
import {
  closePanel,
  connect,
  createWorkflow,
  dropNode,
  fill,
  openNode,
  pick,
  save,
  sourceHandle,
  targetHandle,
} from './support/editor'
import { expect, test, unique } from './support/fixtures'
import { type Session, waitFor } from './support/http'

const runs = async (session: Session, id: number) =>
  (await session.executions(id)).filter((r) => r.status !== 'running')

async function toolbar(page: Page, button: 'Activate' | 'Deactivate', id: number) {
  const answered = page.waitForResponse((r) =>
    r.url().endsWith(`/flow/api/workflows/${id}/${button.toLowerCase()}`)
  )
  await page.getByRole('button', { name: button, exact: true }).click()
  expect((await answered).status()).toBe(200)
}

test('5. an interval schedule fires on the clock, and stops when deactivated', async ({
  page,
  session,
}) => {
  const name = unique('E2E Schedule')
  const id = await createWorkflow(page, name)

  const start = await dropNode(page, 'Trigger', 'Schedule', 'start', { x: 300, y: 60 })
  const log = await dropNode(page, 'Util', 'Log', 'log', { x: 300, y: 260 })
  let panel = await openNode(page, start, 'Schedule Trigger')
  await pick(page, panel, 'Schedule Type', 'Interval')
  await fill(panel, 'Repeat Every', '2')
  await pick(page, panel, 'Repeat Every', 'Seconds')
  const gate = panel
    .locator('label')
    .filter({ hasText: /^Only during market hours$/ })
    .locator('xpath=..')
    .getByRole('switch')
  await gate.click()
  await expect(gate).not.toBeChecked()
  await closePanel(page)
  panel = await openNode(page, log, 'Log')
  await fill(panel, 'Message', 'Scheduled tick at {{time}}')
  await closePanel(page)
  await connect(page, sourceHandle(start), targetHandle(log))
  await save(page, id)
  expect(
    (await session.workflow(id)).nodes.find((n: any) => n.type === 'start').data
  ).toMatchObject({
    scheduleType: 'interval',
    intervalValue: 2,
    intervalUnit: 'seconds',
    marketHoursOnly: false,
  })

  await toolbar(page, 'Activate', id)
  await expect(page.getByText('Workflow active', { exact: true })).toBeVisible()

  // Two runs on the clock, each with the Log node's line.
  await waitFor('two scheduled runs', async () => (await runs(session, id)).length >= 2, 30_000)
  const fired = await runs(session, id)
  for (const run of fired.slice(0, 2)) {
    expect(run.status).toBe('completed')
    expect(JSON.stringify(run.logs)).toMatch(/\[LOG\] Scheduled tick at \d{2}:\d{2}:\d{2}/)
  }
  // Deactivated from the editor's toolbar.
  await page.goto(`/flow/editor/${id}`)
  await toolbar(page, 'Deactivate', id)
  await expect(page.getByText('Workflow inactive', { exact: true })).toBeVisible()
  expect((await session.workflow(id)).is_active).toBe(false)

  // The witness: another schedule, every second.
  const witness = await session.post('/flow/api/workflows/import', {
    name: unique('E2E Witness'),
    nodes: [
      {
        id: 'node_1',
        type: 'start',
        position: { x: 100, y: 100 },
        data: {
          scheduleType: 'interval',
          intervalValue: 1,
          intervalUnit: 'seconds',
          marketHoursOnly: false,
        },
      },
      {
        id: 'node_2',
        type: 'log',
        position: { x: 100, y: 220 },
        data: { message: 'witness', level: 'info' },
      },
    ],
    edges: [{ id: 'e1', source: 'node_1', target: 'node_2' }],
  })
  expect(witness.status).toBe(201)
  const witnessId: number = witness.body.workflow_id
  expect((await session.post(`/flow/api/workflows/${witnessId}/activate`)).status).toBe(200)
  try {
    const witnessed = async (more: number) => {
      const from = (await runs(session, witnessId)).length
      await waitFor(
        `${more} witness runs`,
        async () => (await runs(session, witnessId)).length >= from + more,
        30_000
      )
    }
    // A run that was already under way when it was deactivated lands first.
    await witnessed(2)
    const settled = (await session.executions(id)).length
    await witnessed(3)
    expect((await session.executions(id)).length).toBe(settled)
    // Settled, the Flow list shows its last run (the web's status word).
    await page.goto('/flow')
    await expect(page.getByRole('link', { name: `Open workflow ${name}` })).toContainText(
      'Last: completed'
    )
  } finally {
    await session.post(`/flow/api/workflows/${witnessId}/deactivate`)
  }
})
