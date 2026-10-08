import { afterEach, describe, expect, it, vi } from 'vitest'

vi.mock('@tauri-apps/api/core', () => ({ isTauri: () => false }))

import { createMcpToken, fetchMcpTokens, revokeMcpToken } from './desktop'

type Call = { url: string; init?: RequestInit }

function mockFetch(responses: Record<string, { status?: number; body: unknown }>) {
  const calls: Call[] = []
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url, init })
      const r = responses[url] ?? { status: 404, body: {} }
      return new Response(JSON.stringify(r.body), { status: r.status ?? 200 })
    })
  )
  return calls
}

describe('MCP token helpers', () => {
  afterEach(() => vi.unstubAllGlobals())

  it('creates a token with the CSRF header and returns it once', async () => {
    const calls = mockFetch({
      '/auth/csrf-token': { body: { csrf_token: 'csrf-1' } },
      '/api/mcp/tokens': {
        body: { status: 'success', token: 'oamcp_abc', client_config: { claude_code: 'x' } },
      },
    })
    const res = await createMcpToken('Claude', 'read')
    expect(res.token).toBe('oamcp_abc')
    const post = calls.find((c) => c.url === '/api/mcp/tokens')
    expect(post?.init?.method).toBe('POST')
    expect((post?.init?.headers as Record<string, string>)['X-CSRFToken']).toBe('csrf-1')
    expect(JSON.parse(String(post?.init?.body))).toEqual({ name: 'Claude', scope: 'read' })
  })

  it('surfaces the server message when revoking fails', async () => {
    mockFetch({
      '/auth/csrf-token': { body: { csrf_token: 'c' } },
      '/api/mcp/tokens/9': { status: 404, body: { message: 'That token was already revoked.' } },
    })
    await expect(revokeMcpToken(9)).rejects.toThrow('That token was already revoked.')
  })

  it('lists tokens and tolerates a failed request', async () => {
    mockFetch({ '/api/mcp/tokens': { body: { data: [{ id: 1, name: 'a' }] } } })
    expect(await fetchMcpTokens()).toHaveLength(1)
    mockFetch({})
    expect(await fetchMcpTokens()).toEqual([])
  })
})
