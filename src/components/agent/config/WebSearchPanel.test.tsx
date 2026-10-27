/**
 * The web search panel: what it says the saved configuration does, that a
 * key is write only, and the test result card.
 *
 * Desktop (D-032): the routing sentence names the provider the tool really
 * uses. With Perplexity chosen, link searches go to Tavily whenever a Tavily
 * key is stored; the web's panel said DuckDuckGo.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import type { ReactElement } from 'react'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { WebSearchConfig, WebSearchConfigResponse, WebSearchProvider } from '@/api/agent'
import { render, screen, userEvent } from '@/test/test-utils'
import { WebSearchPanel } from './WebSearchPanel'

let answer: WebSearchConfigResponse
const testProvider = vi.fn()
const setKey = vi.fn()

vi.mock('@/api/agent', async (importOriginal) => ({
  ...(await importOriginal<typeof import('@/api/agent')>()),
  getWebSearchConfig: async () => answer,
  testWebSearchProvider: (provider: string, key?: string) => testProvider(provider, key),
  setWebSearchKey: (provider: string, key: string) => setKey(provider, key),
}))

function provider(
  id: WebSearchProvider['id'],
  overrides: Partial<WebSearchProvider> = {}
): WebSearchProvider {
  const base: Record<string, WebSearchProvider> = {
    duckduckgo: {
      id: 'duckduckgo',
      label: 'DuckDuckGo',
      needs_key: false,
      result_kind: 'links',
      tool: 'web_search',
      description: 'Keyless and the default.',
      has_api_key: false,
      api_key_fingerprint: null,
      api_key_last_used_at: null,
      ready: true,
    },
    tavily: {
      id: 'tavily',
      label: 'Tavily',
      needs_key: true,
      result_kind: 'links',
      tool: 'web_search',
      description: 'A paid search API.',
      has_api_key: false,
      api_key_fingerprint: null,
      api_key_last_used_at: null,
      ready: false,
    },
    perplexity: {
      id: 'perplexity',
      label: 'Perplexity',
      needs_key: true,
      result_kind: 'answer',
      tool: 'web_research',
      description: 'Answers a question from current sources.',
      has_api_key: false,
      api_key_fingerprint: null,
      api_key_last_used_at: null,
      ready: false,
    },
  }
  return { ...base[id], ...overrides }
}

function config(overrides: Partial<WebSearchConfig> = {}): WebSearchConfig {
  return {
    provider: 'duckduckgo',
    perplexity_model: 'openai/gpt-5.6-luna',
    max_calls_per_turn: 5,
    daily_cap: 200,
    usage: { date: '2026-10-27', count: 3, remaining: 197 },
    providers: [provider('duckduckgo'), provider('tavily'), provider('perplexity')],
    ...overrides,
  }
}

function respond(c: WebSearchConfig) {
  answer = {
    data: c,
    defaults: {
      provider: 'duckduckgo',
      perplexity_model: 'openai/gpt-5.6-luna',
      max_calls_per_turn: 5,
      daily_cap: 200,
    },
  }
}

function wrap(node: ReactElement) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(<QueryClientProvider client={client}>{node}</QueryClientProvider>)
}

beforeEach(() => {
  respond(config())
  testProvider.mockReset()
  setKey.mockReset()
})

describe('WebSearchPanel routing', () => {
  it('says DuckDuckGo answers out of the box', async () => {
    wrap(<WebSearchPanel />)
    expect(
      await screen.findByText(
        'Link searches run on DuckDuckGo, which needs no key and works out of the box.'
      )
    ).toBeInTheDocument()
    expect(screen.getByText(/197 of 200/)).toBeInTheDocument()
  })

  it('names Tavily for link searches when Perplexity is chosen and a Tavily key is stored', async () => {
    respond(
      config({
        provider: 'perplexity',
        providers: [
          provider('duckduckgo'),
          provider('tavily', { has_api_key: true, ready: true }),
          provider('perplexity', { has_api_key: true, ready: true }),
        ],
      })
    )
    wrap(<WebSearchPanel />)
    expect(
      await screen.findByText(
        'Perplexity answers questions with citations through the web_research tool. Link searches run on Tavily.'
      )
    ).toBeInTheDocument()
  })

  it('names DuckDuckGo when Perplexity is chosen and no Tavily key is stored', async () => {
    respond(config({ provider: 'perplexity' }))
    wrap(<WebSearchPanel />)
    expect(
      await screen.findByText(
        'Perplexity is selected but no Perplexity key is stored, so web_research cannot run. Link searches run on DuckDuckGo either way.'
      )
    ).toBeInTheDocument()
  })

  it('describes the routing as the tool does it', async () => {
    wrap(<WebSearchPanel />)
    expect(
      await screen.findByText(/choosing it leaves link searches on Tavily when its key is stored/)
    ).toBeInTheDocument()
    expect(screen.queryByText(/leaves link searches on DuckDuckGo\./)).not.toBeInTheDocument()
  })

  it('warns when a cap of zero turns web search off', async () => {
    respond(config({ daily_cap: 0, usage: { date: '2026-10-27', count: 0, remaining: 0 } }))
    wrap(<WebSearchPanel />)
    expect(
      await screen.findByText('The daily cap is 0, so the agent will not search the web at all.')
    ).toBeInTheDocument()
  })
})

describe('WebSearchPanel keys', () => {
  it('shows a stored key as a fingerprint and never as a value', async () => {
    respond(
      config({
        providers: [
          provider('duckduckgo'),
          provider('tavily', {
            has_api_key: true,
            ready: true,
            api_key_fingerprint: '...6789 sha256:0123456789ab',
          }),
          provider('perplexity'),
        ],
      })
    )
    const { container } = wrap(<WebSearchPanel />)
    expect(await screen.findByText(/\.\.\.6789 sha256:0123456789ab/)).toBeInTheDocument()
    for (const input of container.querySelectorAll('input[type="password"]')) {
      expect((input as HTMLInputElement).value).toBe('')
    }
  })

  it('tests a typed key without saving it', async () => {
    testProvider.mockResolvedValue({
      ok: true,
      provider: 'tavily',
      message: 'Tavily answered with 3 results.',
      latency_ms: 420,
      result_count: 3,
      data: config(),
    })
    wrap(<WebSearchPanel />)
    const fields = await screen.findAllByLabelText(/API key/)
    await userEvent.type(fields[0], 'tvly-typed-0123456789')
    const tests = screen.getAllByRole('button', { name: /^Test$/ })
    await userEvent.click(tests[1])
    expect(testProvider).toHaveBeenCalledWith('tavily', 'tvly-typed-0123456789')
    expect(setKey).not.toHaveBeenCalled()
    expect(await screen.findByText('Tavily answered with 3 results.')).toBeInTheDocument()
  })
})
