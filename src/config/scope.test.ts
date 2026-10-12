/**
 * The Python Strategy Host (`/python`) and the Agent (`/agent`, `/agent/config`,
 * the Assistant panel in the /trading terminal) are not part of this version
 * of OpenAlgo Desktop (maintainer, 2026-10-12). Nothing a trader can click may
 * lead to them: no navigation or menu entry, no route, no link.
 */

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { desktopProfileMenuItems } from '@/lib/desktop'
import { bottomNavItems, mobileSheetItems, navItems, profileMenuItems } from './navigation'

const SRC_DIR = join(process.cwd(), 'src')
const REMOVED = /^\/(agent|python)(\/|$)/
const REMOVED_LABEL = /\b(agent|assistant|python)\b/i

/** Every non-test source file under src/. */
function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return sources(path)
    return /\.(ts|tsx)$/.test(name) && !/\.test\.tsx?$/.test(name) ? [path] : []
  })
}

describe('removed from this version: the Python Strategy Host and the Agent', () => {
  it('has no navigation, mobile or profile menu entry for them', () => {
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

  it('declares no /agent or /python route in the router', () => {
    const app = readFileSync(join(SRC_DIR, 'App.tsx'), 'utf8')
    const paths = [...app.matchAll(/<Route\s[^>]*path="([^"]+)"/g)].map((m) => m[1])
    // The parse found the router, so an empty list cannot pass by accident.
    expect(paths).toContain('/trading')
    expect(paths).toContain('*')
    expect(paths.filter((path) => REMOVED.test(path))).toEqual([])
  })

  it('links to neither from any page, card, menu or button', () => {
    const offenders = sources(SRC_DIR).filter((file) =>
      /['"`]\/(agent|python)(\/|['"`?#])/.test(readFileSync(file, 'utf8'))
    )
    expect(offenders).toEqual([])
  })
})
