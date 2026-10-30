/**
 * Parity D-70 and F-104: Flow's toasts belong to the "Flow Workflows" alert
 * category, which the Profile page lists and the alert store turns on by
 * default; turning it off silences them, as on the web.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const sonner = vi.hoisted(() => ({ success: vi.fn(), error: vi.fn(), info: vi.fn() }))
vi.mock('sonner', () => ({ toast: sonner }))

const { useAlertStore } = await import('@/stores/alertStore')
const { showToast } = await import('./toast')

beforeEach(() => {
  for (const fn of Object.values(sonner)) fn.mockReset()
  useAlertStore.getState().resetToDefaults?.()
})

describe('the Flow alert category', () => {
  it('is on by default', () => {
    expect(useAlertStore.getState().categories.flow).toBe(true)
  })

  it('lets Flow toasts through while on and silences them when turned off', () => {
    showToast.success('Workflow saved', 'flow')
    expect(sonner.success).toHaveBeenCalledWith('Workflow saved', undefined)

    useAlertStore.setState((s) => ({ categories: { ...s.categories, flow: false } }))
    showToast.success('Workflow saved', 'flow')
    showToast.error('Workflow not found', 'flow')
    expect(sonner.success).toHaveBeenCalledTimes(1)
    expect(sonner.error).not.toHaveBeenCalled()

    useAlertStore.setState((s) => ({ categories: { ...s.categories, flow: true } }))
  })

  it('is listed on the Profile page as on the web', () => {
    const profile = readFileSync(join(process.cwd(), 'src/pages/Profile.tsx'), 'utf8')
    expect(profile).toContain("key: 'flow',")
    expect(profile).toContain("label: 'Flow Workflows',")
    expect(profile).toContain("description: 'Visual workflow creation, execution, and management',")
    expect(profile).toContain('<strong>Flow:</strong> Visual workflow execution and management')
  })

  it('is the category the Flow pages toast under', () => {
    for (const path of ['pages/flow/FlowIndex.tsx', 'pages/flow/FlowEditor.tsx']) {
      const text = readFileSync(join(process.cwd(), 'src', path), 'utf8')
      const calls = text.match(/showToast\.\w+\(/g)?.length ?? 0
      expect(calls, path).toBeGreaterThan(0)
      expect(text, path).toContain("'flow'")
    }
  })
})
