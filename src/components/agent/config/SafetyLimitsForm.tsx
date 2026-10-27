/**
 * The risk limits, rules and agent behaviour of the Trading safety panel.
 *
 * One form, saved together through `PUT /agent/api/settings`, which checks
 * every value before it writes any: a save with one bad value changes
 * nothing. Only what changed is sent. Each field shows what it reverts to.
 */

import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useId, useState } from 'react'
import {
  type AgentSettings,
  type AgentSettingsUpdate,
  agentErrorMessage,
  agentQueryKeys,
  type ReasoningEffort,
  updateSettings,
} from '@/api/agent'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'

/** The server's cap on the override (D-014), checked here first. */
export const MAX_SYSTEM_PROMPT_CHARS = 20_000
export const SYSTEM_PROMPT_TOO_LONG =
  "The system prompt can be at most 20,000 characters. A longer one does not fit in the agent's instructions, so shorten it and save it again."

type NumberKey =
  | 'max_orders_per_session'
  | 'max_order_quantity'
  | 'max_order_value'
  | 'max_price_deviation_pct'
  | 'duplicate_order_window_seconds'
  | 'max_funds_utilization_pct'
type ListKey = 'allowed_exchanges' | 'allowed_products' | 'symbol_allowlist' | 'symbol_blocklist'
type FlagKey = 'require_analyzer_mode' | 'allow_bulk_destructive'

const NUMBERS: { key: NumberKey; label: string; whole: boolean; max?: number }[] = [
  {
    key: 'max_orders_per_session',
    label: 'Most orders in one conversation (0 stops every order)',
    whole: true,
  },
  { key: 'max_order_quantity', label: 'Largest quantity in one order', whole: true },
  { key: 'max_order_value', label: 'Largest order value, in rupees', whole: false },
  {
    key: 'max_price_deviation_pct',
    label: 'Furthest a limit or trigger price may be from the last price, in percent',
    whole: false,
    max: 100,
  },
  {
    key: 'duplicate_order_window_seconds',
    label: 'Seconds in which the same order again is refused as a duplicate',
    whole: true,
  },
  {
    key: 'max_funds_utilization_pct',
    label: 'Most of your available funds one order may use, in percent',
    whole: false,
    max: 100,
  },
]

const LISTS: { key: ListKey; label: string; hint: string }[] = [
  {
    key: 'allowed_exchanges',
    label: 'Exchanges the agent may trade on',
    hint: 'Separated by commas.',
  },
  { key: 'allowed_products', label: 'Product types it may use', hint: 'CNC, MIS, NRML.' },
  {
    key: 'symbol_allowlist',
    label: 'Only these symbols',
    hint: 'Blank allows every symbol.',
  },
  { key: 'symbol_blocklist', label: 'Never these symbols', hint: 'Separated by commas.' },
]

const FLAGS: { key: FlagKey; label: string; hint: string }[] = [
  {
    key: 'require_analyzer_mode',
    label: 'Only while OpenAlgo is in analyzer mode',
    hint: 'Approved orders then always reach the sandbox. In live mode the agent declines to trade.',
  },
  {
    key: 'allow_bulk_destructive',
    label: 'Allow cancelling every order or closing every position at once',
    hint: 'Off, the agent can still cancel or close one at a time, each with your approval.',
  },
]

const EFFORTS: { value: ReasoningEffort; label: string }[] = [
  { value: 'off', label: "Off (the model's own)" },
  { value: 'low', label: 'Low' },
  { value: 'medium', label: 'Medium' },
  { value: 'high', label: 'High' },
]

type Draft = Record<NumberKey | ListKey | 'kill_switch_file' | 'system_prompt', string> &
  Record<FlagKey, boolean> & { default_reasoning_effort: ReasoningEffort }

function draftOf(s: AgentSettings): Draft {
  return {
    max_orders_per_session: String(s.max_orders_per_session),
    max_order_quantity: String(s.max_order_quantity),
    max_order_value: s.max_order_value,
    max_price_deviation_pct: s.max_price_deviation_pct,
    duplicate_order_window_seconds: String(s.duplicate_order_window_seconds),
    max_funds_utilization_pct: s.max_funds_utilization_pct,
    allowed_exchanges: s.allowed_exchanges.join(', '),
    allowed_products: s.allowed_products.join(', '),
    symbol_allowlist: s.symbol_allowlist.join(', '),
    symbol_blocklist: s.symbol_blocklist.join(', '),
    kill_switch_file: s.kill_switch_file,
    system_prompt: s.system_prompt ?? '',
    require_analyzer_mode: s.require_analyzer_mode,
    allow_bulk_destructive: s.allow_bulk_destructive,
    default_reasoning_effort: s.default_reasoning_effort,
  }
}

/** A list as the server stores it, for comparing. */
const tokens = (text: string) =>
  text
    .split(',')
    .map((t) => t.trim().toUpperCase())
    .filter(Boolean)
    .sort()
    .join(',')

/** The first problem a trader can fix before saving, or null. */
export function draftProblem(d: Draft): string | null {
  for (const n of NUMBERS) {
    const text = d[n.key].trim()
    const value = Number(text)
    const shape = n.whole ? /^\d+$/ : /^\d+(\.\d+)?$/
    if (!shape.test(text) || !Number.isFinite(value)) {
      return `${n.label}: enter ${n.whole ? 'a whole number' : 'a number'} of 0 or more.`
    }
    if (n.max !== undefined && value > n.max) return `${n.label}: enter at most ${n.max}.`
  }
  if (d.system_prompt.trim().length > MAX_SYSTEM_PROMPT_CHARS) return SYSTEM_PROMPT_TOO_LONG
  return null
}

/** Only what changed, as the settings route takes it. */
export function changesOf(d: Draft, s: AgentSettings): AgentSettingsUpdate {
  const base = draftOf(s)
  const out: AgentSettingsUpdate = {}
  for (const n of NUMBERS) {
    if (d[n.key].trim() !== base[n.key]) Object.assign(out, { [n.key]: d[n.key].trim() })
  }
  for (const l of LISTS) {
    if (tokens(d[l.key]) !== tokens(base[l.key])) Object.assign(out, { [l.key]: d[l.key] })
  }
  for (const f of FLAGS) if (d[f.key] !== base[f.key]) out[f.key] = d[f.key]
  if (d.default_reasoning_effort !== base.default_reasoning_effort) {
    out.default_reasoning_effort = d.default_reasoning_effort
  }
  if (d.kill_switch_file.trim() !== base.kill_switch_file)
    out.kill_switch_file = d.kill_switch_file.trim()
  if (d.system_prompt.trim() !== base.system_prompt.trim()) out.system_prompt = d.system_prompt
  return out
}

export function SafetyLimitsForm({
  settings,
  defaults,
}: {
  settings: AgentSettings
  defaults: AgentSettings
}) {
  const id = useId()
  const queryClient = useQueryClient()
  const [draft, setDraft] = useState<Draft>(() => draftOf(settings))
  const [error, setError] = useState<string | null>(null)
  const [saved, setSaved] = useState(false)
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => {
    setSaved(false)
    setDraft((d) => ({ ...d, [key]: value }))
  }
  const changes = changesOf(draft, settings)
  const save = useMutation({
    mutationFn: () => updateSettings(changes),
    onSuccess: (next) => {
      setError(null)
      setSaved(true)
      setDraft(draftOf(next))
      void queryClient.invalidateQueries({ queryKey: agentQueryKeys.settings() })
    },
    onError: (cause) =>
      setError(agentErrorMessage(cause, 'Could not save the trading safety settings')),
  })
  const defaultsDraft = draftOf(defaults)
  const promptLength = draft.system_prompt.trim().length

  return (
    <form
      className="space-y-4 rounded-lg border border-border p-4"
      onSubmit={(event) => {
        event.preventDefault()
        const problem = draftProblem(draft)
        if (problem) {
          setError(problem)
          return
        }
        save.mutate()
      }}
    >
      {FLAGS.map((f) => (
        <div key={f.key} className="flex items-start gap-3">
          <Switch
            id={`${id}-${f.key}`}
            checked={draft[f.key]}
            onCheckedChange={(next) => set(f.key, next)}
            aria-describedby={`${id}-${f.key}-hint`}
          />
          <div className="min-w-0 flex-1">
            <Label htmlFor={`${id}-${f.key}`} className="text-sm font-medium">
              {f.label}
            </Label>
            <p id={`${id}-${f.key}-hint`} className="mt-1 text-xs text-muted-foreground">
              {f.hint}
            </p>
          </div>
        </div>
      ))}

      <div className="grid gap-3 sm:grid-cols-2">
        {NUMBERS.map((n) => (
          <div key={n.key} className="space-y-1.5">
            <Label htmlFor={`${id}-${n.key}`}>{n.label}</Label>
            <Input
              id={`${id}-${n.key}`}
              inputMode={n.whole ? 'numeric' : 'decimal'}
              value={draft[n.key]}
              onChange={(event) => set(n.key, event.target.value)}
            />
            <p className="text-xs text-muted-foreground">Default {defaultsDraft[n.key]}</p>
          </div>
        ))}
        {LISTS.map((l) => (
          <div key={l.key} className="space-y-1.5">
            <Label htmlFor={`${id}-${l.key}`}>{l.label}</Label>
            <Input
              id={`${id}-${l.key}`}
              value={draft[l.key]}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => set(l.key, event.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              {l.hint} Default {defaultsDraft[l.key] || 'none'}.
            </p>
          </div>
        ))}
      </div>

      <div className="space-y-1.5">
        <Label htmlFor={`${id}-kill-file`}>Kill switch file</Label>
        <Input
          id={`${id}-kill-file`}
          value={draft.kill_switch_file}
          autoComplete="off"
          spellCheck={false}
          className="font-mono text-xs"
          onChange={(event) => set('kill_switch_file', event.target.value)}
        />
        <p className="text-xs text-muted-foreground">
          A relative path is inside OpenAlgo's data folder.
        </p>
      </div>

      <div className="space-y-1.5">
        <Label htmlFor={`${id}-effort`}>Default reasoning effort</Label>
        <Select
          value={draft.default_reasoning_effort}
          onValueChange={(next) => set('default_reasoning_effort', next as ReasoningEffort)}
        >
          <SelectTrigger id={`${id}-effort`} className="w-full sm:w-64">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {EFFORTS.map((e) => (
              <SelectItem key={e.value} value={e.value}>
                {e.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <p className="text-xs text-muted-foreground">
          How hard a model that can reason thinks when neither the message nor the model's own
          setting says.
        </p>
      </div>

      <div className="space-y-1.5">
        <Label htmlFor={`${id}-prompt`}>Your own system prompt</Label>
        <Textarea
          id={`${id}-prompt`}
          rows={5}
          value={draft.system_prompt}
          onChange={(event) => set('system_prompt', event.target.value)}
          aria-describedby={`${id}-prompt-hint`}
        />
        <p id={`${id}-prompt-hint`} className="text-xs text-muted-foreground">
          Replaces the built-in instructions. The safety rules and the session details always stay.
          Leave it blank to use the built-in instructions. {promptLength.toLocaleString('en-IN')} of{' '}
          {MAX_SYSTEM_PROMPT_CHARS.toLocaleString('en-IN')} characters.
        </p>
      </div>

      {error && (
        <Alert variant="destructive">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
      {saved && !error && <p className="text-sm text-muted-foreground">Saved.</p>}

      <div className="flex justify-end">
        <Button type="submit" disabled={save.isPending || Object.keys(changes).length === 0}>
          {save.isPending ? 'Saving' : 'Save changes'}
        </Button>
      </div>
    </form>
  )
}
