/**
 * Where Flow sits in the app shell, read from the sources as the web's own
 * tests read them (parity F-100, F-102, F-104, F-407, F-696).
 */

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { Workflow } from 'lucide-react'
import { describe, expect, it } from 'vitest'
import { profileMenuItems } from './navigation'

const SRC = join(process.cwd(), 'src')
const read = (path: string) => readFileSync(join(SRC, path), 'utf8')

/** The <Route> group opened by `<Route element={<Name />}>`, up to its closing tag at the same indent. */
function routeGroup(app: string, layout: string): string {
  const opening = app.match(new RegExp(`\\n( *)<Route element=\\{<${layout} />\\}>`))
  expect(opening, layout).not.toBeNull()
  const start = (opening?.index ?? 0) + 1
  const end = app.indexOf(`\n${opening?.[1]}</Route>`, start)
  expect(end, layout).toBeGreaterThan(start)
  return app.slice(start, end)
}

describe('the Flow routes (F-100)', () => {
  const app = read('App.tsx')

  it('loads the three pages lazily, as the web does', () => {
    expect(app).toContain("const FlowIndex = lazy(() => import('@/pages/flow/FlowIndex'))")
    expect(app).toContain("const FlowEditor = lazy(() => import('@/pages/flow/FlowEditor'))")
    expect(app).toContain(
      "const FlowKeyboardShortcuts = lazy(() => import('@/pages/flow/FlowKeyboardShortcuts'))"
    )
  })

  it('puts the list and the shortcuts in the app layout, the editor full width (no navbar)', () => {
    const layout = routeGroup(app, 'Layout')
    const fullWidth = routeGroup(app, 'FullWidthLayout')
    expect(layout).toContain('<Route path="/flow" element={<FlowIndex />} />')
    expect(layout).toContain('<Route path="/flow/shortcuts" element={<FlowKeyboardShortcuts />} />')
    expect(layout).not.toContain('/flow/editor')
    expect(fullWidth).toContain('<Route path="/flow/editor/:id" element={<FlowEditor />} />')
  })
})

describe('the Flow Editor menu entry (F-102)', () => {
  it('sits after Holdings and before Scalping, with the Workflow icon', () => {
    const hrefs = profileMenuItems.map((item) => item.href)
    const flow = hrefs.indexOf('/flow')
    expect(hrefs[flow - 1]).toBe('/holdings')
    expect(hrefs[flow + 1]).toBe('/scalping')
    expect(profileMenuItems[flow]).toEqual({ href: '/flow', label: 'Flow Editor', icon: Workflow })
  })
})

describe('the Flow alert category on the Profile page (F-104, F-407)', () => {
  it('describes its card as data and workflow automation, without the web Python strategies', () => {
    const profile = read('pages/Profile.tsx')
    const card = profile.slice(profile.indexOf('<CardTitle>Data & Automation</CardTitle>'))
    const description = card.slice(0, card.indexOf('</CardDescription>'))
    expect(description.replace(/\s+/g, ' ')).toContain(
      'Notifications from historical data and workflow automation'
    )
    expect(description).not.toContain('Python')
  })

  it('files the Flow pages copy toasts under clipboard and the rest under flow', () => {
    const index = read('pages/flow/FlowIndex.tsx')
    // The source text, held as a plain string so its ${label} stays literal.
    const copyToast = [
      'showToast.success(`$',
      '{label} copied to clipboard`, ',
      "'clipboard')",
    ].join('')
    expect(index).toContain(copyToast)
    expect(index).toContain("showToast.success('Workflow activated', 'flow')")
  })
})

/** Every non-test source file under a folder. */
function sources(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return sources(path)
    return /\.(ts|tsx)$/.test(name) && !/\.test\.tsx?$/.test(name) ? [path] : []
  })
}

describe('the Flow components stay inside the app (F-696)', () => {
  const files = [...sources(join(SRC, 'components/flow')), ...sources(join(SRC, 'pages/flow'))].map(
    (path) => ({ path: path.slice(SRC.length + 1), text: readFileSync(path, 'utf8') })
  )

  it('use no Tauri API and open no window', () => {
    for (const { path, text } of files) {
      expect(text, path).not.toMatch(/@tauri-apps|window\.open\(/)
    }
  })

  it('name no host but the httpRequest example', () => {
    for (const { path, text } of files) {
      const hosts = [...text.matchAll(/https?:\/\/[^\s'"`)]+/g)].map((m) => m[0])
      expect(hosts, path).toEqual(
        path.endsWith('ConfigPanel.tsx') ? ['https://api.example.com'] : []
      )
    }
  })

  it('touch the clipboard only to copy webhook URLs and secrets', () => {
    const users = files
      .filter(({ text }) => text.includes('navigator.clipboard'))
      .map((f) => f.path)
    expect(users.sort()).toEqual([
      'components/flow/panels/ConfigPanel.tsx',
      'pages/flow/FlowIndex.tsx',
    ])
  })
})
