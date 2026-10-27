import { RefreshCw } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { type ServerComponent, serverSettingsApi } from '@/api/server-settings'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { cn } from '@/lib/utils'

/**
 * Desktop departure from the web's Profile page (docs-35). The web's Server
 * Status card lists its Flask app, WebSocket proxy and ZeroMQ bus with their
 * configured ports, each always "Running". The desktop has no Flask or
 * ZeroMQ, so this card shows the parts the desktop runs, each with the state
 * the server reports (services/system_info/components.rs): its listeners,
 * the broker's streams, the order checks and the health checks. A part that
 * is down says why and what to do.
 *
 * Read when the card opens and on Refresh; the time it was read is shown, so
 * the card never claims more than it knows.
 */

const BADGE: Record<ServerComponent['state'], string> = {
  ok: 'bg-green-600 text-white hover:bg-green-600',
  waiting: 'bg-amber-500 text-white hover:bg-amber-500',
  problem: 'bg-destructive text-white hover:bg-destructive',
  off: '',
}

export function ServerStatusCard() {
  const [parts, setParts] = useState<ServerComponent[] | null>(null)
  const [checkedAt, setCheckedAt] = useState<Date | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    try {
      const settings = await serverSettingsApi.get()
      setParts(settings.components ?? [])
      setCheckedAt(new Date())
      setError(null)
    } catch {
      setError('The server status could not be read. Choose Refresh to try again.')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between gap-4 space-y-0">
        <div className="space-y-1.5">
          <CardTitle>Server Status</CardTitle>
          <CardDescription>
            The parts of OpenAlgo Desktop that are running, and how each one is doing
            {checkedAt ? `, as of ${checkedAt.toLocaleTimeString()}` : ''}.
          </CardDescription>
        </div>
        <Button variant="outline" size="sm" onClick={() => void load()} disabled={loading}>
          <RefreshCw className={cn('h-4 w-4 mr-1', loading && 'animate-spin')} />
          Refresh
        </Button>
      </CardHeader>
      <CardContent>
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : parts === null ? (
          <p className="text-sm text-muted-foreground">Checking...</p>
        ) : (
          <ul className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {parts.map((p) => (
              <li
                key={p.name}
                aria-label={`${p.name}: ${p.label}`}
                className="flex flex-col gap-1 rounded-lg bg-muted p-3"
              >
                <div className="flex items-center justify-between gap-2">
                  <span className="text-sm font-medium">{p.name}</span>
                  <Badge
                    variant={p.state === 'off' ? 'secondary' : 'default'}
                    className={BADGE[p.state]}
                  >
                    {p.label}
                  </Badge>
                </div>
                {p.address ? (
                  <span className="font-mono text-xs text-muted-foreground">{p.address}</span>
                ) : null}
                {p.detail ? (
                  <span className="text-xs text-muted-foreground">{p.detail}</span>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  )
}
