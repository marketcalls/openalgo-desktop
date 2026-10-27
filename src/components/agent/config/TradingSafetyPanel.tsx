/**
 * Trading safety: the settings the web exposes only through its settings API.
 *
 * Desktop addition (decision 1 of the 1.0.4 plan): on the desktop every
 * setting is reachable inside the app, so the kill switch, the risk limits,
 * the analyzer-only rule, the bulk cancel and close permission, the system
 * prompt override and the default reasoning effort get a screen. The values
 * travel through the web's own `PUT /agent/api/settings`, so what the screen
 * writes is exactly what an API client writes. What each limit refuses is the
 * order guard's; this panel stores and shows them.
 *
 * The kill switch is first because it is the one control a trader reaches for
 * in a hurry. It sets both halves together, the setting and the file, and it
 * reads either one engaged: a file created outside OpenAlgo stops the agent
 * too, and the panel says so.
 */

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'
import { agentErrorMessage, agentQueryKeys, getSettings } from '@/api/agent'
import { getKillSwitch, killSwitchQueryKey, setKillSwitch } from '@/api/agentSafety'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { SafetyLimitsForm } from './SafetyLimitsForm'

function KillSwitch() {
  const queryClient = useQueryClient()
  const [error, setError] = useState<string | null>(null)
  const state = useQuery({ queryKey: killSwitchQueryKey, queryFn: getKillSwitch })
  const save = useMutation({
    mutationFn: setKillSwitch,
    onSuccess: (next) => {
      setError(null)
      queryClient.setQueryData(killSwitchQueryKey, next)
      void queryClient.invalidateQueries({ queryKey: agentQueryKeys.settings() })
    },
    onError: (cause) => setError(agentErrorMessage(cause, 'Could not change the kill switch')),
  })

  if (state.isPending) return <Skeleton className="h-20 w-full" />
  if (state.isError) {
    return (
      <Alert variant="destructive">
        <AlertDescription>
          {agentErrorMessage(state.error, 'The kill switch could not be read')}
        </AlertDescription>
      </Alert>
    )
  }
  const s = state.data
  const stuckOnFile = s.by_file && !s.by_setting

  return (
    <div className="space-y-3 rounded-lg border border-border p-4">
      <div className="flex items-start gap-3">
        <Switch
          id="agent-kill-switch"
          checked={s.engaged}
          disabled={save.isPending}
          onCheckedChange={(next) => save.mutate(next)}
          aria-describedby="agent-kill-switch-help"
        />
        <div className="min-w-0 flex-1">
          <Label htmlFor="agent-kill-switch" className="text-sm font-medium">
            Kill switch: stop every agent order
          </Label>
          <p id="agent-kill-switch-help" className="mt-1 text-sm text-muted-foreground">
            {s.engaged
              ? 'Engaged. The agent places, modifies and cancels nothing, whatever you approve. Its read-only answers still work.'
              : 'Released. Orders the agent proposes still need your approval and must pass the limits below.'}
          </p>
          <p className="mt-1 break-all text-xs text-muted-foreground">
            It is also engaged while this file exists, so you can stop the agent without opening
            OpenAlgo: <span className="font-mono">{s.path}</span>
          </p>
        </div>
      </div>
      {stuckOnFile && (
        <Alert variant="warning">
          <AlertDescription>
            The file is there, so the switch stays engaged. Delete{' '}
            <span className="break-all font-mono">{s.path}</span> to release it.
          </AlertDescription>
        </Alert>
      )}
      {error && (
        <Alert variant="destructive">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
    </div>
  )
}

export function TradingSafetyPanel() {
  const settings = useQuery({ queryKey: agentQueryKeys.settings(), queryFn: getSettings })

  return (
    <section aria-labelledby="agent-safety-heading" className="space-y-4">
      <div>
        <h2 id="agent-safety-heading" className="text-base font-semibold">
          Trading safety
        </h2>
        <p className="mt-1 text-sm text-muted-foreground">
          The limits every order the agent proposes must pass, checked again after you approve it.
          When your API key's order mode is Semi-Auto, an order you approve in the chat is not sent:
          it waits in the Action Center until you place it there.
        </p>
      </div>
      <KillSwitch />
      {settings.isPending ? (
        <Skeleton className="h-64 w-full" />
      ) : settings.isError ? (
        <Alert variant="destructive">
          <AlertDescription>
            {agentErrorMessage(settings.error, 'The trading safety settings could not be loaded')}
          </AlertDescription>
        </Alert>
      ) : (
        <SafetyLimitsForm settings={settings.data.data} defaults={settings.data.defaults} />
      )}
    </section>
  )
}
