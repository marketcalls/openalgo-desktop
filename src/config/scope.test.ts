/**
 * The Python Strategy Host (`/python`) is not part of this version of OpenAlgo
 * Desktop (maintainer, 2026-10-12). Nothing a trader can click may lead to it:
 * no navigation or menu entry, no route, no link.
 *
 * The Agent (`/agent`) came back in 1.0.4 (maintainer, 2026-10-27), as a page
 * with its route, its server path and its navigation entry in one change.
 * Flow (`/flow`) came in 1.0.5 (maintainer, 2026-10-29) the same way.
 */

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { desktopProfileMenuItems } from '@/lib/desktop'
import { bottomNavItems, mobileSheetItems, navItems, profileMenuItems } from './navigation'

const SRC_DIR = join(process.cwd(), 'src')
const REMOVED = /^\/python(\/|$)/
const REMOVED_LABEL = /\bpython\b/i

/** Every non-test source file under src/. */
function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return sources(path)
    return /\.(ts|tsx)$/.test(name) && !/\.test\.tsx?$/.test(name) ? [path] : []
  })
}

function routerPaths(): string[] {
  const app = readFileSync(join(SRC_DIR, 'App.tsx'), 'utf8')
  return [...app.matchAll(/<Route\s[^>]*path="([^"]+)"/g)].map((m) => m[1])
}

describe('removed from this version: the Python Strategy Host', () => {
  it('has no navigation, mobile or profile menu entry for it', () => {
    const menus = {
      navItems,
      bottomNavItems,
      mobileSheetItems,
      profileMenuItems,
      desktopProfileMenuItems,
    }
    for (const [menu, items] of Object.entries(menus)) {
      for (const item of items) {
        expect(item.href, `${menu}: ${item.label}`).not.toMatch(REMOVED)
        expect(item.label, `${menu}: ${item.href}`).not.toMatch(REMOVED_LABEL)
      }
    }
  })

  it('declares no /python route in the router', () => {
    const paths = routerPaths()
    // The parse found the router, so an empty list cannot pass by accident.
    expect(paths).toContain('/trading')
    expect(paths).toContain('*')
    expect(paths.filter((path) => REMOVED.test(path))).toEqual([])
  })

  it('links to it from no page, card, menu or button', () => {
    const offenders = sources(SRC_DIR).filter((file) =>
      /['"`]\/python(\/|['"`?#])/.test(readFileSync(file, 'utf8'))
    )
    expect(offenders).toEqual([])
  })
})

describe('the Agent is a page of this version', () => {
  it('has its route and its profile menu entry, as on the web', () => {
    expect(routerPaths()).toContain('/agent')
    expect(profileMenuItems.map((item) => item.href)).toContain('/agent')
    expect(profileMenuItems.find((item) => item.href === '/agent')?.label).toBe('Agent')
  })
})

describe('Flow is a page of this version', () => {
  it('has the three routes and the profile menu entry the web has', () => {
    const paths = routerPaths()
    expect(paths.filter((path) => path.startsWith('/flow')).sort()).toEqual([
      '/flow',
      '/flow/editor/:id',
      '/flow/shortcuts',
    ])
    expect(profileMenuItems.find((item) => item.href === '/flow')?.label).toBe('Flow Editor')
  })
})
