/**
 * The journeys' `test`: every journey starts from a current sign-in to the
 * trader's account on the real server the global setup started (made again
 * once 03:00 IST has passed, and never with the boundary inside a journey:
 * support/signin.ts), its page starts with that session, and `session` makes
 * the same JSON calls the app's pages make, signing in again on a 401.
 */

import { test as base, expect, type Page } from '@playwright/test'
import { Session } from './http'
import { type SignedIn, SignIns, traderFromSetup } from './signin'

function fromSetup(name: string): string {
  const value = process.env[name]
  if (!value) throw new Error(`${name} is not set: run with playwright.flow.config.ts`)
  return value
}

export const test = base.extend<{ session: Session; signedIn: SignedIn }, { signIns: SignIns }>({
  // biome-ignore lint/correctness/noEmptyPattern: a Playwright fixture's first argument is a pattern
  signIns: [
    async ({}, use) => {
      await use(new SignIns(fromSetup('OPENALGO_E2E_BASE'), traderFromSetup()))
    },
    { scope: 'worker' },
  ],
  // biome-ignore lint/correctness/noEmptyPattern: a Playwright fixture's first argument is a pattern
  baseURL: async ({}, use) => {
    await use(fromSetup('OPENALGO_E2E_BASE'))
  },
  // Its own timeout: near the boundary it waits until the boundary has passed.
  signedIn: [
    async ({ signIns }, use) => {
      await use(await signIns.current())
    },
    { timeout: 330_000 },
  ],
  storageState: async ({ signedIn }, use) => {
    await use(signedIn)
  },
  session: async ({ page, signIns }, use) => {
    await use(
      new Session(page.request, async () => {
        await page.context().addCookies((await signIns.renew()).cookies)
      })
    )
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
