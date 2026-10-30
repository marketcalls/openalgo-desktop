import { AlertTriangle } from 'lucide-react'
import { Alert, AlertDescription } from '@/components/ui/alert'

/** Hosts that only this computer can reach. */
const LOCAL_HOSTS = new Set(['localhost', '0.0.0.0', '[::1]'])

/**
 * True when a webhook URL names this computer's own address, which a service
 * on the internet (TradingView, Chartink) cannot send to. The same hosts as
 * the server's `is_localhost` (`/api/config/host`), read from the URL itself
 * so the note always matches the address on screen.
 */
export function isLocalWebhookUrl(url: string): boolean {
  let host: string
  try {
    host = new URL(url).hostname.toLowerCase()
  } catch {
    return false
  }
  return LOCAL_HOSTS.has(host) || host.startsWith('127.')
}

/**
 * Desktop departure from the web's Flow pages. The web shows a workflow's
 * webhook URL on whatever HOST_SERVER the server was set up with, usually a
 * public address. On the desktop the URL is this computer's address
 * (http://127.0.0.1:5000) until the trader turns on the built-in ngrok tunnel
 * or enters a Host Server URL, and a URL on that address silently never
 * receives an alert. The note says so, in the words the desktop's
 * TradingView, GoCharting and Chartink pages already use.
 */
export function FlowWebhookReachability({
  webhookUrl,
  compact = false,
}: {
  webhookUrl: string | undefined
  compact?: boolean
}) {
  if (!webhookUrl || !isLocalWebhookUrl(webhookUrl)) return null
  return (
    <Alert variant="destructive" className={compact ? 'p-2' : undefined}>
      <AlertTriangle className="h-4 w-4" />
      <AlertDescription className={compact ? 'text-[10px] leading-snug' : undefined}>
        <span>
          <strong>Webhook URL not accessible!</strong> TradingView, Chartink and other services
          cannot send alerts to this computer's own address. Turn on the{' '}
          <strong>built-in ngrok tunnel</strong> on the Broker tab of your Profile, or use{' '}
          <strong>Cloudflare Tunnel</strong>, <strong>VS Code Dev Tunnel</strong> or a{' '}
          <strong>custom domain</strong> and enter that address as the Host Server URL there.
        </span>
      </AlertDescription>
    </Alert>
  )
}
