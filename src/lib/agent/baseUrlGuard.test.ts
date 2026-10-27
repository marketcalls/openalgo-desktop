import { describe, expect, it } from 'vitest'
import cases from '../../../src-tauri/tests/fixtures/agent/base_url_cases.json'
import { BASE_URL_MESSAGES, baseUrlProblem, hostClass } from './baseUrlGuard'

interface Case {
  url: string
  kind: string
  error: string | null
  class?: string
}

// The server's sentence for a missing endpoint names the kind; the dialogs
// say it in their own words, so those cases check only that one is refused.
const requiredByServer = (error: string | null) => error?.endsWith('requires a base URL') ?? false

describe('the base URL guard the server and both dialogs share', () => {
  for (const c of (cases as { cases: Case[] }).cases) {
    if (c.error === null && !c.url.trim()) continue
    it(`${JSON.stringify(c.url)} for ${c.kind}`, () => {
      const got = baseUrlProblem(c.url)
      if (requiredByServer(c.error)) expect(got).toBe(BASE_URL_MESSAGES.required)
      else expect(got).toBe(c.error)
      if (c.class) expect(hostClass(new URL(c.url.trim()))).toBe(c.class)
    })
  }
})
