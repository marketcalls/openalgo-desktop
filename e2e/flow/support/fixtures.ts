/**
 * The journeys' `test`: every page starts signed in as the trader the global
 * setup prepared, on the real server it started, and `session` makes the
 * same JSON calls the app's pages make.
 */

import { test as base, expect, type Page } from '@playwright/test'
import { Session } from './http'

function fromSetup(name: string): string {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is not set: run with playwright.flow.config.ts`)
  return value
}

export const test = base.extend<{ session: Session }>({
  // biome-ignore lint/correctness/noEmptyPattern: a Playwright fixture's first argument is a pattern
  baseURL: async ({}, use) => {
    await use(fromSetup('OPENALGO_E2E_BASE'))
  },
  // biome-ignore lint/correctness/noEmptyPattern: a Playwright fixture's first argument is a pattern
  storageState: async ({}, use) => {
    await use(fromSetup('OPENALGO_E2E_STATE'))
  },
  session: async ({ page }, use) => {
    await use(new Session(page.request))
  },
})

export { expect }

/** A name no other journey or earlier run uses. */
export function unique(prefix: string): string {
  return `${prefix} ${Date.now().toString(36)}`
}

/** The workflow id of the editor page that is open. */
export function editorId(page: Page): number {
  const m = /\/flow\/editor\/(\d+)/.exec(page.url())
  if (!m) throw new Error(`Not on a workflow editor: ${page.url()}`)
  return Number(m[1])
}
