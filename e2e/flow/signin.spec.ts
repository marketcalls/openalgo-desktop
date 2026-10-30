/**
 * The journeys' sign-in helper (support/signin.ts, decisions.md 28): no
 * journey starts close enough to 03:00 IST to span it, and a session that has
 * been signed out (as the boundary signs out every earlier login) signs in
 * again and the call goes through.
 */

import { expect, test } from './support/fixtures'
import { CSRF_HEADER, csrfToken } from './support/http'
import {
  crossedBoundary,
  GUARD_BEFORE_MS,
  msUntilBoundary,
  SETTLE_AFTER_MS,
  type SignedIn,
  SignIns,
  waitBeforeSignIn,
} from './support/signin'

/** An instant given as IST wall time (UTC+05:30, no daylight saving). */
function ist(day: number, hour: number, minute: number, second = 0): number {
  return Date.UTC(2026, 9, day, hour, minute, second) - 330 * 60_000
}

const s = (n: number) => n * 1000

test('28. the boundary guard: the next 03:00 IST, and the wait near it', () => {
  // 03:00 IST is 21:30 UTC the day before.
  expect(msUntilBoundary(Date.UTC(2026, 9, 29, 21, 30))).toBe(0)
  expect(msUntilBoundary(ist(30, 3, 0))).toBe(0)
  expect(msUntilBoundary(ist(30, 2, 59))).toBe(s(60))
  expect(msUntilBoundary(ist(30, 10, 0))).toBe(s(17 * 3600))
  // Between midnight and 03:00 the next boundary is today's.
  expect(msUntilBoundary(ist(30, 0, 30))).toBe(s(2.5 * 3600))
  expect(msUntilBoundary(ist(29, 23, 59))).toBe(s(3 * 3600 + 60))

  // Far from it: sign in at once.
  expect(waitBeforeSignIn(ist(30, 10, 0))).toBe(0)
  expect(waitBeforeSignIn(ist(30, 0, 30))).toBe(0)
  expect(waitBeforeSignIn(ist(30, 3, 0) - GUARD_BEFORE_MS - 1000)).toBe(0)
  // Close before it: wait until it and the expiry poll after it have passed.
  expect(waitBeforeSignIn(ist(30, 3, 0) - GUARD_BEFORE_MS)).toBe(GUARD_BEFORE_MS + SETTLE_AFTER_MS)
  expect(waitBeforeSignIn(ist(30, 2, 59))).toBe(s(60) + SETTLE_AFTER_MS)
  // At it and just after it: until the poll has run.
  expect(waitBeforeSignIn(ist(30, 3, 0))).toBe(SETTLE_AFTER_MS)
  expect(waitBeforeSignIn(ist(30, 3, 0, 30))).toBe(SETTLE_AFTER_MS - s(30))
  expect(waitBeforeSignIn(ist(30, 3, 0) + SETTLE_AFTER_MS)).toBe(0)
  // No journey is longer than the guard.
  expect(GUARD_BEFORE_MS).toBeGreaterThan(test.info().timeout)
})

test('28. a sign-in is reused until 03:00 IST has passed, then made again', async () => {
  // Signed in at 10:00 IST: still current at 02:59 the next day, not at 03:00.
  expect(crossedBoundary(ist(29, 10, 0), ist(30, 2, 59))).toBe(false)
  expect(crossedBoundary(ist(29, 10, 0), ist(30, 3, 0))).toBe(true)
  expect(crossedBoundary(ist(29, 10, 0), ist(31, 1, 0))).toBe(true)
  // Signed in between midnight and 03:00: the boundary that same night.
  expect(crossedBoundary(ist(30, 1, 0), ist(30, 2, 59))).toBe(false)
  expect(crossedBoundary(ist(30, 1, 0), ist(30, 3, 1))).toBe(true)
  // Signed in just after it: current until the next one.
  expect(crossedBoundary(ist(30, 3, 1), ist(30, 23, 0))).toBe(false)

  // The worker's sign-ins, on a clock the test moves and a stand-in login.
  let now = ist(29, 10, 0)
  const made: number[] = []
  const signIns = new SignIns(
    'http://127.0.0.1:1',
    { username: 'trader', password: 'unused' },
    () => now,
    async () => {
      made.push(now)
      return { cookies: [], origins: [] } as SignedIn
    }
  )
  await signIns.current()
  now = ist(29, 15, 30)
  await signIns.current()
  now = ist(30, 2, 59)
  await signIns.current()
  expect(made).toEqual([ist(29, 10, 0)])
  now = ist(30, 3, 5)
  await signIns.current()
  await signIns.current()
  expect(made).toEqual([ist(29, 10, 0), ist(30, 3, 5)])
  // A call that met a signed-out session signs in again at once.
  await signIns.renew()
  expect(signIns.made).toBe(3)
})

test('28. a signed-out session signs in again and the call goes through', async ({
  page,
  session,
}) => {
  // Signed in for this journey (the worker's current sign-in).
  expect((await session.get('/flow/api/workflows')).status).toBe(200)
  expect(session.reauths).toBe(0)

  // Signed out, as the boundary signs out every earlier login: the app's
  // own sign-out, which also ends the broker session and locks the keys.
  const out = await page.request.post('/auth/logout', {
    headers: { [CSRF_HEADER]: await csrfToken(page.request), Accept: 'application/json' },
  })
  expect(out.status()).toBe(200)
  const refused = await page.request.get('/flow/api/workflows', {
    headers: { Accept: 'application/json' },
  })
  expect(refused.status()).toBe(401)

  // The helper signs in again, broker and analyzer mode included, and the
  // call goes through.
  const again = await session.get('/flow/api/workflows')
  expect(again.status).toBe(200)
  expect(Array.isArray(again.body)).toBe(true)
  expect(session.reauths).toBe(1)
  const status = (await session.get('/auth/session-status')).body
  expect(status).toMatchObject({ authenticated: true, logged_in: true, broker: 'dhan_sandbox' })
  expect((await session.get('/auth/analyzer-mode')).body?.data?.analyze_mode).toBe(true)

  // The page shares the new session.
  await page.goto('/flow')
  await expect(page.getByRole('heading', { name: 'Flow Editor' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Switch to Live mode' })).toBeVisible()
  expect(session.reauths).toBe(1)
})
