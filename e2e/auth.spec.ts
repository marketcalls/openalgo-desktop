import { expect, type Page, test } from '@playwright/test'

/**
 * These run against `vite preview`, which has no backend: every call to the
 * server is answered with the app's index page, so the app sees no session.
 * A test that needs a signed-in session stubs `/auth/session-status`.
 */

test.describe('Authentication Flow', () => {
  test('should show login page', async ({ page }) => {
    await page.goto('/login')
    await page.waitForLoadState('networkidle')

    // Check that page loaded
    await expect(page.locator('body')).toBeVisible()
  })

  test('should have password input on login', async ({ page }) => {
    await page.goto('/login')
    await page.waitForLoadState('networkidle')

    await expect(page.locator('input[type="password"]')).toBeVisible()
  })

  test('accessing protected routes requires authentication', async ({ page }) => {
    await page.goto('/dashboard')
    await page.waitForLoadState('networkidle')

    // Sent to sign in (or to first-time setup), and the signed-in shell
    // never rendered.
    await expect(page).toHaveURL(/\/(login|setup)(\?|$)/)
    await expect(page.locator('input[type="password"]')).toBeVisible()
    await expect(page.getByTestId('navbar-row')).toHaveCount(0)
  })

  // The check above must be able to fail: with a signed-in session the same
  // navigation stays on the dashboard and shows the signed-in shell, which is
  // exactly what the unauthenticated test asserts does not happen.
  test('a signed-in session reaches the dashboard', async ({ page }) => {
    await signedIn(page)
    await page.goto('/dashboard')

    await expect(page.getByTestId('navbar-row')).toBeVisible()
    await expect(page).toHaveURL(/\/dashboard$/)
  })
})

test.describe('Reset Password Flow', () => {
  test('should load reset password page', async ({ page }) => {
    await page.goto('/reset-password')
    await page.waitForLoadState('networkidle')

    await expect(page.locator('body')).toBeVisible()
  })
})

async function signedIn(page: Page) {
  await page.route('**/auth/session-status', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        status: 'success',
        logged_in: true,
        authenticated: true,
        broker: 'angel',
        username: 'e2e',
        active_sessions: 1,
      }),
    })
  )
}
