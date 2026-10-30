/**
 * Journey 7 (decisions.md 14, desktop departures from the web): deleting a
 * workflow asks first, and Cancel keeps it; leaving the editor with unsaved
 * changes asks first, by the back arrow and by the browser's Back, and Stay
 * keeps the edits on screen.
 */

import { canvasNodes, createWorkflow, dropNode, save } from './support/editor'
import { expect, test, unique } from './support/fixtures'

test('7. delete asks first; Cancel keeps the workflow, Delete removes it', async ({
  page,
  session,
}) => {
  const name = unique('E2E Delete')
  const created = await session.post('/flow/api/workflows', { name })
  expect(created.status).toBe(201)
  const id: number = created.body.id

  await page.goto('/flow')
  const card = page.getByRole('link', { name: `Open workflow ${name}` })
  const askDelete = async () => {
    await card.getByRole('button', { name: 'Workflow actions' }).click()
    await page.getByRole('menuitem', { name: 'Delete' }).click()
    const confirm = page.getByRole('alertdialog')
    await expect(confirm.getByRole('heading', { name: `Delete ${name}?` })).toBeVisible()
    return confirm
  }

  let confirm = await askDelete()
  await confirm.getByRole('button', { name: 'Cancel' }).click()
  await expect(confirm).toHaveCount(0)
  await expect(card).toBeVisible()
  expect((await session.get(`/flow/api/workflows/${id}`)).status).toBe(200)

  confirm = await askDelete()
  const deleted = page.waitForResponse(
    (r) => r.request().method() === 'DELETE' && r.url().endsWith(`/flow/api/workflows/${id}`)
  )
  await confirm.getByRole('button', { name: 'Delete workflow' }).click()
  expect((await deleted).status()).toBe(200)
  await expect(card).toHaveCount(0)
  expect((await session.get(`/flow/api/workflows/${id}`)).status).toBe(404)
})

test('7. leaving the editor with unsaved changes asks first', async ({ page, session }) => {
  const name = unique('E2E Unsaved')
  const id = await createWorkflow(page, name)
  await dropNode(page, 'Trigger', 'Schedule', 'start', { x: 300, y: 80 })
  await save(page, id)

  // An unsaved edit: a second node on the canvas.
  await dropNode(page, 'Util', 'Log', 'log', { x: 300, y: 260 })
  await expect(page.getByText('Unsaved', { exact: true })).toBeVisible()
  const guard = page.getByRole('alertdialog').filter({ hasText: 'Leave without saving?' })

  // The back arrow: Stay keeps the editor and the edit.
  await page.getByRole('button', { name: 'Back to flows list' }).click()
  await expect(guard).toBeVisible()
  await guard.getByRole('button', { name: 'Stay' }).click()
  await expect(guard).toHaveCount(0)
  expect(page.url()).toMatch(new RegExp(`/flow/editor/${id}$`))
  await expect(canvasNodes(page)).toHaveCount(2)
  await expect(page.getByText('Unsaved', { exact: true })).toBeVisible()

  // The browser's Back: asked too, and Stay keeps it.
  await page.evaluate(() => window.history.back())
  await expect(guard).toBeVisible()
  await guard.getByRole('button', { name: 'Stay' }).click()
  await expect(guard).toHaveCount(0)
  await expect(canvasNodes(page)).toHaveCount(2)

  // Leave without saving: on the list, and the stored graph is the saved one.
  await page.getByRole('button', { name: 'Back to flows list' }).click()
  await guard.getByRole('button', { name: 'Leave without saving' }).click()
  await page.waitForURL(/\/flow$/)
  expect((await session.workflow(id)).nodes.map((n: any) => n.type)).toEqual(['start'])

  // Saved changes leave without a question.
  await page.goto(`/flow/editor/${id}`)
  await expect(canvasNodes(page)).toHaveCount(1)
  await page.getByRole('button', { name: 'Back to flows list' }).click()
  await page.waitForURL(/\/flow$/)
  await expect(guard).toHaveCount(0)
})
