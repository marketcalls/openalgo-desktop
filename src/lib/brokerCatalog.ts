/**
 * Desktop: every broker OpenAlgo Desktop signs in to, with the name the FAQ
 * shows. The web's FAQ says 36 broker plugins and leaves Rupeezy out,
 * although the web supports it (`broker/rupeezy`); the FAQ here takes its
 * count and its list from this table, and `brokerCatalog.test.ts` fails when
 * the table and the server's catalogue (`src-tauri/src/brokers/catalog.rs`,
 * `ALL_BROKERS`) differ (docs-39).
 */
export interface SupportedBroker {
  /** The catalogue id, as in `/<id>/callback`. */
  id: string
  name: string
  /** Trades crypto rather than Indian markets. */
  crypto?: true
}

/** Alphabetical by name, the order the FAQ lists them in. */
export const SUPPORTED_BROKERS: readonly SupportedBroker[] = [
  { id: 'fivepaisa', name: '5 Paisa' },
  { id: 'fivepaisaxts', name: '5 Paisa (XTS)' },
  { id: 'aliceblue', name: 'Alice Blue' },
  { id: 'angel', name: 'Angel One' },
  { id: 'arrow', name: 'Arrow' },
  { id: 'compositedge', name: 'CompositEdge' },
  { id: 'definedge', name: 'Definedge' },
  { id: 'deltaexchange', name: 'Delta Exchange', crypto: true },
  { id: 'dhan', name: 'Dhan' },
  { id: 'dhan_sandbox', name: 'Dhan (Sandbox)' },
  { id: 'firstock', name: 'Firstock' },
  { id: 'flattrade', name: 'Flattrade' },
  { id: 'fyers', name: 'Fyers' },
  { id: 'groww', name: 'Groww' },
  { id: 'hdfcsecurities', name: 'HDFC Securities' },
  { id: 'hdfcsky', name: 'HDFC Sky' },
  { id: 'ibulls', name: 'Ibulls' },
  { id: 'iifl', name: 'IIFL' },
  { id: 'iiflcapital', name: 'IIFL Capital' },
  { id: 'indmoney', name: 'IndMoney' },
  { id: 'jainamxts', name: 'JainamXts' },
  { id: 'kotak', name: 'Kotak Securities' },
  { id: 'motilal', name: 'Motilal Oswal' },
  { id: 'mstock', name: 'mStock by Mirae Asset' },
  { id: 'nubra', name: 'Nubra' },
  { id: 'paytm', name: 'Paytm Money' },
  { id: 'pocketful', name: 'Pocketful' },
  { id: 'rmoney', name: 'RMoney' },
  { id: 'rupeezy', name: 'Rupeezy' },
  { id: 'samco', name: 'Samco' },
  { id: 'shoonya', name: 'Shoonya (Finvasia)' },
  { id: 'tradesmart', name: 'TradeSmart' },
  { id: 'tradejini', name: 'Tradejini' },
  { id: 'upstox', name: 'Upstox' },
  { id: 'wisdom', name: 'Wisdom Capital' },
  { id: 'zebu', name: 'Zebu' },
  { id: 'zerodha', name: 'Zerodha' },
]

/** How many brokers there are in all. */
export const BROKER_COUNT = SUPPORTED_BROKERS.length

/** How many of them trade Indian markets (all but the crypto exchange). */
export const SECURITIES_BROKER_COUNT = SUPPORTED_BROKERS.filter((b) => !b.crypto).length

/** "A, B and C": the names in order, for a sentence. */
export function brokerNamesSentence(): string {
  const names = SUPPORTED_BROKERS.map((b) => b.name)
  return `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`
}
