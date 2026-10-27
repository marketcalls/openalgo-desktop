/**
 * Desktop: the built-in ngrok tunnel on the Profile Broker tab, inside the
 * web's Server Configuration card. The web runs ngrok with the authtoken in
 * ngrok's own configuration file and its domain from HOST_SERVER; here the
 * trader saves the authtoken (encrypted, never shown again) and an optional
 * reserved domain, and sees the tunnel's status. The card's Enable Ngrok
 * Tunnel switch turns it on and off.
 */

import { useCallback, useEffect, useState } from 'react'
import { type TunnelChange, type TunnelSettings, type TunnelState, tunnelApi } from '@/api/tunnel'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { showToast } from '@/utils/toast'

/** How often the status is read while the card is open. */
export const TUNNEL_STATUS_REFRESH_MS = 4000

const STATE_LABEL: Record<TunnelState, string> = {
  stopped: 'Stopped',
  connecting: 'Connecting',
  online: 'Online',
  error: 'Error',
}

const STATE_CLASS: Record<TunnelState, string> = {
  stopped: 'text-muted-foreground',
  connecting: 'text-yellow-600',
  online: 'text-green-600',
  error: 'text-red-600',
}

export function NgrokTunnelSettings() {
  const [settings, setSettings] = useState<TunnelSettings | null>(null)
  const [readFailed, setReadFailed] = useState(false)
  const [authtoken, setAuthtoken] = useState('')
  const [domain, setDomain] = useState('')
  const [busy, setBusy] = useState(false)

  const load = useCallback(async (resetDomain: boolean) => {
    try {
      const s = await tunnelApi.get()
      setSettings(s)
      setReadFailed(false)
      if (resetDomain) setDomain(s.domain ?? '')
    } catch {
      // Keep what was shown; say it could not be read now.
      setReadFailed(true)
    }
  }, [])

  useEffect(() => {
    void load(true)
    const timer = window.setInterval(() => void load(false), TUNNEL_STATUS_REFRESH_MS)
    return () => window.clearInterval(timer)
  }, [load])

  const domainEdited = domain.trim() !== (settings?.domain ?? '')
  const changed = authtoken.trim() !== '' || domainEdited

  const apply = async (change: TunnelChange, done: string) => {
    setBusy(true)
    try {
      const s = await tunnelApi.save(change)
      setSettings(s)
      setReadFailed(false)
      setAuthtoken('')
      setDomain(s.domain ?? '')
      showToast.success(done, 'admin')
    } catch (e) {
      showToast.error(e instanceof Error ? e.message : 'Could not save. Try again.', 'admin')
    } finally {
      setBusy(false)
    }
  }

  const handleSave = () => {
    const change: TunnelChange = {}
    if (authtoken.trim()) change.authtoken = authtoken.trim()
    if (domainEdited) change.domain = domain.trim()
    void apply(change, 'Tunnel settings saved.')
  }

  const status = settings?.tunnel
  const state = status?.state ?? 'stopped'

  return (
    <section aria-labelledby="ngrok-tunnel-title" className="space-y-3 rounded-md border p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 id="ngrok-tunnel-title" className="text-sm font-medium">
          Built-in ngrok tunnel
        </h3>
        <Badge variant="outline" className={STATE_CLASS[state]} data-testid="tunnel-state">
          {STATE_LABEL[state]}
        </Badge>
      </div>
      <p className="text-sm" role="status" aria-live="polite" data-testid="tunnel-message">
        {status?.message ?? 'Reading the tunnel status.'}
        {readFailed && ' (Could not read the latest status; showing the last one.)'}
      </p>
      {settings?.domain && (
        <p className="text-xs text-muted-foreground" data-testid="tunnel-addresses">
          With Enable Ngrok Tunnel on, your Host Server URL is https://{settings.domain}, online or
          not: webhook addresses use it, and so does the broker redirect URL. Register https://
          {settings.domain}/&lt;broker&gt;/callback with your broker, or keep a fixed local redirect
          URL on Broker Configuration.
        </p>
      )}

      <div className="space-y-2">
        <Label htmlFor="ngrok-authtoken">ngrok authtoken</Label>
        <Input
          id="ngrok-authtoken"
          type="password"
          autoComplete="off"
          spellCheck={false}
          value={authtoken}
          onChange={(e) => setAuthtoken(e.target.value)}
          placeholder={
            settings?.authtoken_saved
              ? 'Saved. Paste a new one to replace it.'
              : 'Paste it from Your Authtoken in your ngrok dashboard'
          }
        />
        {settings?.authtoken_saved && (
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-xs text-muted-foreground">An authtoken is saved.</span>
            <Button
              type="button"
              variant="link"
              size="sm"
              className="h-auto p-0"
              disabled={busy}
              onClick={() =>
                void apply({ forget_authtoken: true }, 'The saved authtoken was removed.')
              }
            >
              Remove the saved authtoken
            </Button>
          </div>
        )}
      </div>

      <div className="space-y-2">
        <Label htmlFor="ngrok-domain">ngrok domain</Label>
        <Input
          id="ngrok-domain"
          spellCheck={false}
          value={domain}
          onChange={(e) => setDomain(e.target.value)}
          placeholder="yourname.ngrok-free.app"
        />
        <p className="text-xs text-muted-foreground">
          The free domain listed under Domains in your ngrok dashboard. The tunnel always opens on
          it, so your addresses never change.
        </p>
      </div>

      <Button type="button" variant="outline" onClick={handleSave} disabled={!changed || busy}>
        {busy ? 'Saving...' : 'Save authtoken and domain'}
      </Button>
    </section>
  )
}
