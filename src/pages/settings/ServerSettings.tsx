/**
 * Desktop-only: Server Settings.
 *
 * Where OpenAlgo web takes its listen addresses from .env (FLASK_HOST_IP,
 * FLASK_PORT, WEBSOCKET_HOST, WEBSOCKET_PORT), the desktop app keeps them in
 * its own settings and changes them here. Shaped like the web's settings pages
 * (Leverage, the admin pages): one card, a form, Save and Retry.
 */

import { AlertTriangle, Loader2, RefreshCw, Save, Server } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import {
  type MarketDataStatus,
  type ServerSettings as Settings,
  serverSettingsApi,
} from '@/api/server-settings'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { showToast } from '@/utils/toast'

const LOOPBACK = '127.0.0.1'
const ALL_INTERFACES = '0.0.0.0'

interface FormState {
  http_host: string
  http_port: string
  ws_host: string
  ws_port: string
  lan_enabled: boolean
}

function toForm(settings: Settings): FormState {
  return {
    http_host: settings.http_host,
    http_port: String(settings.http_port),
    ws_host: settings.ws_host,
    ws_port: String(settings.ws_port),
    lan_enabled: settings.lan_enabled,
  }
}

function parsePort(value: string): number | null {
  if (!/^\d+$/.test(value.trim())) return null
  const port = Number(value)
  return port >= 1024 && port <= 65535 ? port : null
}

/** A sentence for the trader, or null when the form can be saved. */
export function validateServerSettings(form: FormState): string | null {
  const httpPort = parsePort(form.http_port)
  const wsPort = parsePort(form.ws_port)
  if (httpPort === null) return 'Enter an app port between 1024 and 65535.'
  if (wsPort === null) return 'Enter a market data port between 1024 and 65535.'
  if (httpPort === wsPort) return 'The app port and the market data port must be different.'
  if (!form.http_host.trim() || !form.ws_host.trim()) {
    return 'Enter an address for both the app and the market data feed.'
  }
  return null
}

/**
 * The feed could not start and the server says why. A taken port counts only
 * while it is still the saved market data port: after the trader saves
 * another one, the feed moves to it by itself.
 */
export function feedProblem(status: MarketDataStatus | undefined, wsPort?: string): boolean {
  if (!status?.message) return false
  if (status.state === 'failed') return true
  return status.state === 'port_in_use' && (wsPort === undefined || String(status.port) === wsPort)
}

export default function ServerSettings() {
  const [saved, setSaved] = useState<FormState | null>(null)
  const [form, setForm] = useState<FormState | null>(null)
  const [isLoading, setIsLoading] = useState(true)
  const [isSaving, setIsSaving] = useState(false)
  const [fetchError, setFetchError] = useState(false)
  const [feedStatus, setFeedStatus] = useState<MarketDataStatus | undefined>()

  const fetchCurrent = useCallback(async () => {
    setIsLoading(true)
    setFetchError(false)
    try {
      const settings = await serverSettingsApi.get()
      setFeedStatus(settings.ws_status)
      const current = toForm(settings)
      setSaved(current)
      setForm(current)
    } catch {
      setFetchError(true)
      showToast.error('Could not load the server settings. Try again.')
    } finally {
      setIsLoading(false)
    }
  }, [])

  useEffect(() => {
    fetchCurrent()
  }, [fetchCurrent])

  const update = (patch: Partial<FormState>) => setForm((f) => (f ? { ...f, ...patch } : f))

  const setLan = (enabled: boolean) => {
    if (!form) return
    // Swap the loopback default for "all interfaces" and back, but leave an
    // address the user typed themselves alone.
    const swap = (host: string) => {
      if (enabled && host === LOOPBACK) return ALL_INTERFACES
      if (!enabled && host === ALL_INTERFACES) return LOOPBACK
      return host
    }
    update({ lan_enabled: enabled, http_host: swap(form.http_host), ws_host: swap(form.ws_host) })
  }

  const handleSave = async () => {
    if (!form) return
    const problem = validateServerSettings(form)
    if (problem) {
      showToast.error(problem)
      return
    }
    setIsSaving(true)
    try {
      const res = await serverSettingsApi.save({
        http_host: form.http_host.trim(),
        http_port: Number(form.http_port),
        ws_host: form.ws_host.trim(),
        ws_port: Number(form.ws_port),
        lan_enabled: form.lan_enabled,
      })
      if (res.status === 'success') {
        const next = res.data ? toForm(res.data) : form
        if (res.data) setFeedStatus(res.data.ws_status)
        setSaved(next)
        setForm(next)
        showToast.success(res.message || 'Server settings saved.')
      } else {
        showToast.error(res.message || 'The server settings were not saved. Try again.')
      }
    } catch (error) {
      const message = error instanceof Error && error.message ? error.message : ''
      showToast.error(message || 'The server settings were not saved. Try again.')
    } finally {
      setIsSaving(false)
    }
  }

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-[400px]">
        <Loader2 className="h-8 w-8 animate-spin text-muted-foreground" />
      </div>
    )
  }

  const isModified =
    form !== null && saved !== null && JSON.stringify(form) !== JSON.stringify(saved)
  const portChanged = form !== null && saved !== null && form.http_port !== saved.http_port

  return (
    <div className="container mx-auto py-6 space-y-6 max-w-2xl">
      <Card>
        <CardHeader>
          <div className="flex items-center gap-3">
            <Server className="h-6 w-6 text-primary" />
            <div>
              <CardTitle>Server Settings</CardTitle>
              <CardDescription>
                Where OpenAlgo Desktop listens for the app, the API, broker logins and the market
                data feed. Your trading platforms, the Python SDK and broker redirect URLs use these
                addresses.
              </CardDescription>
            </div>
          </div>
        </CardHeader>
        <CardContent>
          {feedProblem(feedStatus, saved?.ws_port) && (
            <Alert variant="destructive" className="mb-6">
              <AlertTriangle className="h-4 w-4" />
              <AlertTitle>Live market data is not running</AlertTitle>
              <AlertDescription>{feedStatus?.message}</AlertDescription>
            </Alert>
          )}
          {fetchError || !form ? (
            <div className="space-y-4">
              <p className="text-sm text-muted-foreground">
                The server settings could not be loaded.
              </p>
              <Button size="sm" variant="destructive" onClick={fetchCurrent}>
                <RefreshCw className="h-4 w-4 mr-1" />
                Retry
              </Button>
            </div>
          ) : (
            <form
              className="space-y-6"
              onSubmit={(e) => {
                e.preventDefault()
                handleSave()
              }}
            >
              <div className="flex items-start justify-between gap-4 rounded-lg border p-4">
                <div className="space-y-1">
                  <Label htmlFor="lan-enabled">Allow access from other devices</Label>
                  <p className="text-xs text-muted-foreground">
                    Off keeps OpenAlgo Desktop reachable only from this computer. Turn it on to
                    reach it from another device on your network.
                  </p>
                </div>
                <Switch id="lan-enabled" checked={form.lan_enabled} onCheckedChange={setLan} />
              </div>

              {form.lan_enabled && (
                <Alert variant="warning">
                  <AlertTriangle className="h-4 w-4" />
                  <AlertTitle>Anyone on your network can reach the login page</AlertTitle>
                  <AlertDescription>
                    Traffic to other devices is not encrypted: use this only on a network you trust,
                    or reach OpenAlgo through a tunnel with https. Use a strong password and turn on
                    two-factor login in your profile. Leave this off on public or shared networks.
                  </AlertDescription>
                </Alert>
              )}

              <div className="grid gap-4 sm:grid-cols-2">
                <div className="space-y-2">
                  <Label htmlFor="http-host">App address</Label>
                  <Input
                    id="http-host"
                    value={form.http_host}
                    onChange={(e) => update({ http_host: e.target.value })}
                    autoComplete="off"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="http-port">App port</Label>
                  <Input
                    id="http-port"
                    inputMode="numeric"
                    value={form.http_port}
                    onChange={(e) => update({ http_port: e.target.value })}
                    autoComplete="off"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="ws-host">Market data address</Label>
                  <Input
                    id="ws-host"
                    value={form.ws_host}
                    onChange={(e) => update({ ws_host: e.target.value })}
                    autoComplete="off"
                  />
                </div>
                <div className="space-y-2">
                  <Label htmlFor="ws-port">Market data port</Label>
                  <Input
                    id="ws-port"
                    inputMode="numeric"
                    value={form.ws_port}
                    onChange={(e) => update({ ws_port: e.target.value })}
                    autoComplete="off"
                  />
                </div>
              </div>

              {portChanged && (
                <Alert variant="warning">
                  <AlertTriangle className="h-4 w-4" />
                  <AlertTitle>Update your broker app and trading platforms</AlertTitle>
                  <AlertDescription>
                    A new app port changes the redirect URL your broker app must use and the address
                    your trading platforms and the Python SDK connect to. Update them after saving.
                  </AlertDescription>
                </Alert>
              )}

              <div className="flex items-center gap-2">
                <Button type="submit" disabled={isSaving || !isModified}>
                  {isSaving ? (
                    <Loader2 className="h-4 w-4 animate-spin mr-1" />
                  ) : (
                    <Save className="h-4 w-4 mr-1" />
                  )}
                  Save
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  disabled={isSaving || !isModified}
                  onClick={() => setForm(saved)}
                >
                  Discard changes
                </Button>
              </div>
            </form>
          )}
        </CardContent>
      </Card>
    </div>
  )
}
