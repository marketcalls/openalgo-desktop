/**
 * Driving the Flow editor as a trader does: the palette, the canvas, the
 * node's configuration panel, the handles and the toolbar.
 */

import { expect, type Locator, type Page } from '@playwright/test'

/** The palette's tabs (components/flow/panels/NodePalette.tsx). */
export type PaletteTab = 'Trigger' | 'Action' | 'Data' | 'Stream' | 'Logic' | 'Util'

export function canvasNodes(page: Page): Locator {
  return page.locator('.react-flow__node')
}

export function canvasEdges(page: Page): Locator {
  return page.locator('.react-flow__edge')
}

/** A node on the canvas by its type; `nth` in the order the nodes were added. */
export function nodeOfType(page: Page, type: string, nth = 0): Locator {
  return page.locator(`.react-flow__node-${type}`).nth(nth)
}

/** From the Flow list: New Workflow, its name, Create; lands in the editor. */
export async function createWorkflow(page: Page, name: string): Promise<number> {
  await page.goto('/flow')
  await page.getByRole('button', { name: 'New Workflow' }).click()
  await page.getByLabel('Workflow Name', { exact: true }).fill(name)
  await page.getByRole('dialog').getByRole('button', { name: 'Create', exact: true }).click()
  await page.waitForURL(/\/flow\/editor\/\d+$/)
  await expect(page.locator('.react-flow__pane')).toBeVisible()
  await expect(page.getByLabel('Workflow name', { exact: true })).toHaveValue(name)
  return Number(/\/flow\/editor\/(\d+)/.exec(page.url())![1])
}

/**
 * Drags a node from the palette onto the canvas at `at` (pane pixels, the
 * node's top-left corner) and returns it.
 */
export async function dropNode(
  page: Page,
  tab: PaletteTab,
  label: string,
  type: string,
  at: { x: number; y: number }
): Promise<Locator> {
  const before = await page.locator(`.react-flow__node-${type}`).count()
  await page.getByRole('tab', { name: tab, exact: true }).click()
  await page
    .getByRole('button', { name: `Add ${label} node`, exact: true })
    .dragTo(page.locator('.react-flow__pane'), { targetPosition: at })
  const added = nodeOfType(page, type, before)
  await expect(added).toBeVisible()
  return added
}

/** The open node's configuration panel. */
export function configPanel(page: Page): Locator {
  return page.locator('div.w-80').filter({ has: page.getByText('Configure node', { exact: true }) })
}

/** Opens a node's panel by clicking the node. */
export async function openNode(page: Page, node: Locator, title: string): Promise<Locator> {
  await node.click()
  const panel = configPanel(page)
  await expect(panel.getByRole('heading', { name: title, exact: true })).toBeVisible()
  return panel
}

export async function closePanel(page: Page): Promise<void> {
  await page.getByRole('button', { name: 'Close configuration panel' }).click()
  await expect(configPanel(page)).toHaveCount(0)
}

/** The block a field's label heads (the label's parent element). */
function fieldBlock(panel: Locator, label: string): Locator {
  return panel
    .locator('label')
    .filter({ hasText: new RegExp(`^${label}$`) })
    .first()
    .locator('xpath=..')
}

/** A text, number or multi-line field by its label. */
export function field(panel: Locator, label: string): Locator {
  return fieldBlock(panel, label).locator('input, textarea').first()
}

export async function fill(panel: Locator, label: string, value: string): Promise<void> {
  const input = field(panel, label)
  await input.fill(value)
  await expect(input).toHaveValue(value)
}

/** A dropdown field by its label: opens it and picks `option`. */
export async function pick(
  page: Page,
  panel: Locator,
  label: string,
  option: string
): Promise<void> {
  const trigger = fieldBlock(panel, label).getByRole('combobox').first()
  await trigger.click()
  await page.getByRole('option', { name: option, exact: true }).click()
  await expect(trigger).toHaveText(option)
}

/** The value a dropdown field shows. */
export function picked(panel: Locator, label: string): Locator {
  return fieldBlock(panel, label).getByRole('combobox').first()
}

/** Drags a connection from a source handle to a target handle. */
export async function connect(page: Page, from: Locator, to: Locator): Promise<void> {
  const before = await canvasEdges(page).count()
  const a = await from.boundingBox()
  const b = await to.boundingBox()
  if (!a || !b) throw new Error('A handle is not on screen')
  await page.mouse.move(a.x + a.width / 2, a.y + a.height / 2)
  await page.mouse.down()
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2, { steps: 15 })
  await page.mouse.up()
  await expect(canvasEdges(page)).toHaveCount(before + 1)
}

export function sourceHandle(node: Locator, id?: string): Locator {
  return id
    ? node.locator(`.react-flow__handle.source[data-handleid="${id}"]`)
    : node.locator('.react-flow__handle.source').first()
}

export function targetHandle(node: Locator): Locator {
  return node.locator('.react-flow__handle.target').first()
}

/**
 * Toasts stack over the top right of the window, where the editor's toolbar
 * buttons and the first workflow cards' menus are, and a pointer resting on
 * one keeps it open. Before pressing a control there, move the pointer off
 * and let the toasts go, as a trader would wait for them.
 */
export async function clearToasts(page: Page): Promise<void> {
  await page.mouse.move(1, 1)
  await expect(page.locator('[data-sonner-toast]')).toHaveCount(0, { timeout: 20_000 })
}

/** The toolbar's Save, waiting for the stored answer. */
export async function save(page: Page, id: number): Promise<void> {
  await expect(page.getByText('Unsaved', { exact: true })).toBeVisible()
  const stored = page.waitForResponse(
    (r) => r.request().method() === 'PUT' && r.url().endsWith(`/flow/api/workflows/${id}`)
  )
  await clearToasts(page)
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  expect((await stored).status()).toBe(200)
  await expect(page.getByText('Unsaved', { exact: true })).toHaveCount(0)
}

/** The editor's actions menu (the toolbar's three dots). */
export async function editorAction(page: Page, item: string): Promise<void> {
  await clearToasts(page)
  await page.getByRole('button', { name: 'Workflow actions' }).click()
  await page.getByRole('menuitem', { name: item }).click()
}

/** Run Now from the actions menu; resolves with the execute route's answer. */
export async function runNow(page: Page, id: number): Promise<any> {
  const answered = page.waitForResponse(
    (r) => r.request().method() === 'POST' && r.url().endsWith(`/flow/api/workflows/${id}/execute`)
  )
  await editorAction(page, 'Run Now')
  const response = await answered
  expect(response.status()).toBe(200)
  return response.json()
}

/** The Execution Log panel and its entries' messages. */
export function executionLog(page: Page): Locator {
  return page.locator('div.w-80').filter({ has: page.getByText('Execution Log', { exact: true }) })
}
