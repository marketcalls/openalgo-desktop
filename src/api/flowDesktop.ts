/**
 * Desktop-only: the webhook URLs a trader copies from Flow.
 *
 * The web shows each workflow's webhook URL on the HOST_SERVER it was set up
 * with. The desktop has a built-in ngrok tunnel instead, so while the tunnel
 * is online the URLs shown are on its public address (what TradingView or
 * Chartink can reach), and otherwise the server's own (the Host Server URL,
 * or this computer's address, which components/desktop/FlowWebhookReachability
 * then flags). Kept out of api/flow.ts so that file stays the web's.
 */

import { getWebhookInfo, type WebhookInfo } from './flow'
import { type TunnelStatus, tunnelApi } from './tunnel'

/** The origin of an absolute URL, or null when it is not one. */
function originOf(url: string | null | undefined): string | null {
  if (!url) return null
  try {
    return new URL(url).origin
  } catch {
    return null
  }
}

/**
 * The same webhook settings with every URL on the tunnel's public address
 * while the tunnel is online; unchanged otherwise. Only the origin moves: the
 * path, the `{symbol}` placeholder and the `?secret=` query stay as the
 * server wrote them.
 */
export function onTunnelAddress(info: WebhookInfo, tunnel: TunnelStatus | null): WebhookInfo {
  const to = tunnel?.state === 'online' ? originOf(tunnel.url) : null
  const from = originOf(info.webhook_url)
  if (!to || !from || to === from) return info
  const move = (url: string) => (url.startsWith(from) ? to + url.slice(from.length) : url)
  return {
    ...info,
    webhook_url: move(info.webhook_url),
    webhook_url_with_symbol: move(info.webhook_url_with_symbol),
    webhook_url_with_secret: info.webhook_url_with_secret && move(info.webhook_url_with_secret),
  }
}

/**
 * A workflow's webhook settings as the trader should copy them. A tunnel
 * status that cannot be read leaves the server's URLs as they are: the
 * settings still show, and the reachability note says if they are local.
 */
export async function getWebhookInfoForTrader(id: number): Promise<WebhookInfo> {
  const [info, settings] = await Promise.all([
    getWebhookInfo(id),
    tunnelApi.get().catch(() => null),
  ])
  return onTunnelAddress(info, settings?.tunnel ?? null)
}
