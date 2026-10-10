import AxeBuilder from '@axe-core/playwright'
import { expect, type Page, test } from '@playwright/test'

/**
 * Axe scans that fail on what they find.
 *
 * A violation of impact "serious" or "critical" fails the test unless its rule
 * is in ALLOWED below with the reason it was accepted. Lower impacts are
 * logged, not failed. An entry in ALLOWED is a reviewed decision, not a way to
 * make a red run green: name the pages it covers and why it cannot be fixed
 * now, and remove it once it is.
 */

type Impact = 'minor' | 'moderate' | 'serious' | 'critical'

const FAILING_IMPACTS: ReadonlySet<Impact> = new Set(['serious', 'critical'])

/** Rule id -> why it is accepted for now. */
const ALLOWED: Readonly<Record<string, string>> = {}

interface Finding {
  id: string
  impact: string | null | undefined
  targets: string[]
}

/** The serious and critical violations not accepted in ALLOWED. */
function blocking(
  violations: Awaited<ReturnType<AxeBuilder['analyze']>>['violations']
): Finding[] {
  return violations
    .filter((v) => FAILING_IMPACTS.has(v.impact as Impact) && !(v.id in ALLOWED))
    .map((v) => ({
      id: v.id,
      impact: v.impact,
      targets: v.nodes.map((n) => n.target.join(' ')),
    }))
}

async function scan(page: Page, configure: (axe: AxeBuilder) => AxeBuilder) {
  const results = await configure(new AxeBuilder({ page })).analyze()
  const logged = results.violations.filter((v) => !FAILING_IMPACTS.has(v.impact as Impact))
  if (logged.length > 0) {
    console.log(
      'Accessibility findings below the failing impact:',
      logged.map((v) => `${v.id} (${v.impact}): ${v.nodes.length} element(s)`)
    )
  }
  return blocking(results.violations)
}

const wcag = (axe: AxeBuilder) => axe.withTags(['wcag2a', 'wcag2aa'])

test.describe('Accessibility', () => {
  test('home page accessibility scan', async ({ page }) => {
    await page.goto('/')
    await page.waitForLoadState('networkidle')

    expect(await scan(page, wcag)).toEqual([])
  })

  test('login page accessibility scan', async ({ page }) => {
    await page.goto('/login')
    await page.waitForLoadState('networkidle')

    expect(await scan(page, wcag)).toEqual([])
  })

  test('color contrast check', async ({ page }) => {
    await page.goto('/')
    await page.waitForLoadState('networkidle')

    expect(await scan(page, (axe) => axe.withRules(['color-contrast']))).toEqual([])
  })

  // The check itself must be able to fail: a nameless button is a critical
  // "button-name" violation, so the same scan that passes the real page has
  // to report it once one is added.
  test('the scan reports a violation it is shown', async ({ page }) => {
    await page.goto('/login')
    await page.waitForLoadState('networkidle')
    await page.evaluate(() => {
      const button = document.createElement('button')
      button.id = 'axe-self-test'
      document.body.appendChild(button)
    })

    const found = await scan(page, wcag)
    expect(found).toContainEqual(
      expect.objectContaining({ id: 'button-name', targets: ['#axe-self-test'] })
    )
  })
})

test.describe('Accessibility - Mobile', () => {
  test.use({ viewport: { width: 375, height: 667 } })

  test('mobile layout accessibility scan', async ({ page }) => {
    await page.goto('/')
    await page.waitForLoadState('networkidle')

    expect(await scan(page, wcag)).toEqual([])
  })
})
