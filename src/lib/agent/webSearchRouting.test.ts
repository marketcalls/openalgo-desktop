/**
 * Desktop (D-032): the provider the panel names for a link search the
 * selected one cannot take is the one the tool asks
 * (`src-tauri/src/agent/websearch/tests/chain.rs`).
 */

import { describe, expect, it } from 'vitest'
import type { WebSearchConfig, WebSearchProvider, WebSearchProviderId } from '@/api/agent'
import { linkFallback } from './webSearchRouting'

const LABELS: Record<WebSearchProviderId, string> = {
  duckduckgo: 'DuckDuckGo',
  tavily: 'Tavily',
  perplexity: 'Perplexity',
  firecrawl: 'Firecrawl',
  tinyfish: 'TinyFish',
}

function provider(id: WebSearchProviderId, keyed: WebSearchProviderId[]): WebSearchProvider {
  const has = keyed.includes(id)
  return {
    id,
    label: LABELS[id],
    needs_key: id !== 'duckduckgo',
    result_kind: id === 'perplexity' ? 'answer' : 'links',
    tool: id === 'perplexity' ? 'web_research' : 'web_search',
    description: '',
    fetch: id === 'firecrawl' || id === 'tinyfish',
    has_api_key: has,
    api_key_fingerprint: null,
    api_key_last_used_at: null,
    ready: id === 'duckduckgo' || has,
  }
}

function config(selected: WebSearchProviderId, keyed: WebSearchProviderId[]): WebSearchConfig {
  const ids: WebSearchProviderId[] = ['duckduckgo', 'tavily', 'perplexity', 'firecrawl', 'tinyfish']
  return {
    provider: selected,
    fetch_provider: 'firecrawl',
    fetch_ready: keyed.includes('firecrawl') || keyed.includes('tinyfish'),
    preferred_sources: [],
    perplexity_model: 'openai/gpt-5.6-luna',
    max_calls_per_turn: 10,
    daily_cap: 200,
    usage: { date: '2026-11-26', count: 0, remaining: 200 },
    providers: ids.map((id) => provider(id, keyed)),
  }
}

describe('linkFallback', () => {
  it('takes the keyed providers in the order the tool tries them', () => {
    expect(linkFallback(config('perplexity', ['tinyfish', 'firecrawl']))).toBe('Firecrawl')
    expect(linkFallback(config('perplexity', ['tinyfish', 'tavily']))).toBe('Tavily')
  })

  it('never names the selected provider, which has no key when this is asked', () => {
    expect(linkFallback(config('firecrawl', ['tinyfish']))).toBe('TinyFish')
    expect(linkFallback(config('tavily', ['firecrawl']))).toBe('Firecrawl')
  })

  it('names DuckDuckGo when no other keyed provider holds a key', () => {
    expect(linkFallback(config('tinyfish', []))).toBe('DuckDuckGo')
    expect(linkFallback(config('perplexity', ['perplexity']))).toBe('DuckDuckGo')
  })
})
