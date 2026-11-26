/**
 * Desktop (D-032): who answers a link search the selected provider cannot
 * take, said the way the agent's web_search tool decides it
 * (`src-tauri/src/agent/tools/websearch/search.rs`).
 *
 * The Web search panel (`components/agent/config/WebSearchPanel.tsx`) is
 * carried over from the web, which says choosing Perplexity, or a keyed
 * provider without its key, leaves link searches on DuckDuckGo. The web's
 * tool, and the desktop's, send them to the first of Tavily, Firecrawl and
 * TinyFish that has a key, and DuckDuckGo after them. The panel calls these
 * at the few places where the web names DuckDuckGo, so it can still follow
 * the web by diff.
 *
 * A DuckDuckGo selection keeps link searches on DuckDuckGo, as the panel
 * says; a keyed provider gets one only when DuckDuckGo failed.
 */

import type { WebSearchConfig } from '@/api/agent'

/** The keyed link providers, in the order the tool tries them. */
const KEYED_LINK_PROVIDERS = ['tavily', 'firecrawl', 'tinyfish'] as const

/**
 * The provider that answers a link search the selected one cannot take: the
 * first of Tavily, Firecrawl and TinyFish with a stored key, other than the
 * selected one, and DuckDuckGo after them.
 */
export function linkFallback(config: WebSearchConfig): string {
  for (const id of KEYED_LINK_PROVIDERS) {
    const entry = config.providers.find((provider) => provider.id === id)
    if (entry && id !== config.provider && entry.has_api_key) return entry.label
  }
  return 'DuckDuckGo'
}

/** The card description's sentence on what choosing Perplexity does to link searches. */
export const PERPLEXITY_LEAVES_LINK_SEARCHES =
  'Perplexity answers questions with citations through a separate tool, so choosing it leaves ' +
  'link searches on the first of Tavily, Firecrawl and TinyFish that has a key, and on ' +
  'DuckDuckGo otherwise.'

/**
 * Where link searches go once a keyed provider's key is removed, put where
 * the web's sentence says "DuckDuckGo": which provider that is depends on
 * the keys still stored, so it is said in general.
 */
export const LINK_FALLBACK_AFTER_REMOVAL = 'another provider with a key, or to DuckDuckGo,'
