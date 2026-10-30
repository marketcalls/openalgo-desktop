/**
 * Desktop-only helpers for the Flow editor, kept out of the files carried
 * from the web so those still take web changes by diff. Each fixes a web
 * defect named in .lane/105/parity.md.
 */

/**
 * The output that carries a branching node's "condition held" path.
 *
 * Parity F-693: the web's edge "+" menu inserts a Logic node and wires its
 * output to the old target with no source handle. These nodes only have
 * branch outputs, so the wire cannot be drawn, and the run treats a
 * handle-less edge out of a branching node as pass-through: the order after
 * it ran whether or not the condition held. The new wire leaves from the
 * pass branch instead (timeCondition and the NOT gate name it "yes").
 */
const PASS_BRANCH: Readonly<Record<string, string>> = {
  timeCondition: 'yes',
  notGate: 'yes',
  positionCheck: 'true',
  fundCheck: 'true',
  priceCondition: 'true',
  timeWindow: 'true',
  varCondition: 'true',
  priceAlert: 'true',
  andGate: 'true',
  orGate: 'true',
}

/** The pass branch of a branching node type; undefined for a node with one output. */
export function passBranchHandle(nodeType: string): string | undefined {
  return PASS_BRANCH[nodeType]
}

/**
 * Parity F-545: iron condor and both spreads place fixed sides whatever the
 * node's Action (the executor's _generate_strategy_legs), so the panel does
 * not caption them "Long strategy" / "Short strategy".
 */
export const FIXED_SIDE_STRATEGIES: ReadonlySet<string> = new Set([
  'iron_condor',
  'bull_call_spread',
  'bear_put_spread',
])

/**
 * The History node's intervals. Parity F-564: the web offered five (1m, 5m,
 * 15m, 1h and Daily stored as `1d`, which the OpenAlgo history API does not
 * accept: daily is `D`), so a daily History node failed on most brokers and
 * every other interval the API takes had no choice. The desktop offers the
 * API's whole list (`SUPPORTED_INTERVALS`, src-tauri/src/services/schemas.rs),
 * with the web's five labels kept. A stored value not in the list (a web
 * export's `1d`) is offered as it is, so the Select never shows blank over a
 * value the node still holds.
 */
const HISTORY_INTERVALS: ReadonlyArray<{ value: string; label: string }> = [
  { value: '1s', label: '1 Sec' },
  { value: '5s', label: '5 Sec' },
  { value: '10s', label: '10 Sec' },
  { value: '15s', label: '15 Sec' },
  { value: '30s', label: '30 Sec' },
  { value: '45s', label: '45 Sec' },
  { value: '1m', label: '1 Min' },
  { value: '2m', label: '2 Min' },
  { value: '3m', label: '3 Min' },
  { value: '5m', label: '5 Min' },
  { value: '10m', label: '10 Min' },
  { value: '15m', label: '15 Min' },
  { value: '20m', label: '20 Min' },
  { value: '30m', label: '30 Min' },
  { value: '1h', label: '1 Hour' },
  { value: '2h', label: '2 Hours' },
  { value: '3h', label: '3 Hours' },
  { value: '4h', label: '4 Hours' },
  { value: 'D', label: 'Daily' },
  { value: 'W', label: 'Weekly' },
  { value: 'M', label: 'Monthly' },
  { value: 'Q', label: 'Quarterly' },
  { value: 'Y', label: 'Yearly' },
]

export function historyIntervalOptions(current: unknown) {
  const stored = typeof current === 'string' ? current.trim() : ''
  if (!stored || HISTORY_INTERVALS.some((o) => o.value === stored)) return HISTORY_INTERVALS
  return [{ value: stored, label: `${stored} (stored on this node)` }, ...HISTORY_INTERVALS]
}

/**
 * Decision 32: the History node's face shows the interval and range the run
 * uses (src-tauri/src/flow/nodes/history.rs), not the web's `1d` and
 * `30 days` fallbacks: 5 Min when no interval is stored, a stored `1d` as
 * Daily (the run sends it as `D`), any other unlisted value as stored.
 */
export function historyFaceInterval(value: unknown): string {
  const stored = typeof value === 'string' ? value.trim() : ''
  const sent = stored === '1d' ? 'D' : stored || '5m'
  return HISTORY_INTERVALS.find((o) => o.value === sent)?.label ?? stored
}

/**
 * The range the run asks for: a start and end date when both are stored,
 * else the last N days, else nothing, which stops the node before the broker
 * (decision 20), so the face says the days are required.
 */
export function historyFaceRange(data: {
  days?: unknown
  startDate?: unknown
  endDate?: unknown
}): string {
  const start = typeof data.startDate === 'string' ? data.startDate.trim() : ''
  const end = typeof data.endDate === 'string' ? data.endDate.trim() : ''
  if (start && end) return `${start} to ${end}`
  const { days } = data
  if (typeof days === 'string' && days.includes('{{')) return `${days.trim()} days`
  const count = typeof days === 'number' || typeof days === 'string' ? Number(days) : 0
  return count > 0 ? `${count} days` : 'Days required'
}

/**
 * Parity F-207: the web's message for a run that finishes in the background
 * says its result "appears in the workflow's execution history", a view
 * neither the web nor the desktop has. This says where the trader does see
 * it.
 */
export const FLOW_STARTED_IN_BACKGROUND =
  'Workflow started. It waits on a Delay or Wait Until step and finishes in the background: its orders go out when the wait ends and show in the order book.'

/** One line of the editor's run log (components/flow/panels/ExecutionLogPanel's LogEntry). */
interface RunLogLine {
  time: string
  message: string
  level: 'info' | 'error' | 'warning'
}

/**
 * The run log to show for a Run Now request that failed.
 *
 * Parity F-156: the execute route answers a failed run with an error status
 * and the full per-node log in the body, but the request then rejects, and
 * the web's editor showed only the summary sentence, throwing the log away.
 * The log is shown when the reply carries one; otherwise the one sentence.
 */
export function failedRunLog(error: Error, now: Date = new Date()): RunLogLine[] {
  const body = (error as { response?: { data?: { logs?: unknown } } }).response?.data
  const logs = body?.logs
  if (Array.isArray(logs) && logs.length > 0) {
    const lines = logs as RunLogLine[]
    const last = lines[lines.length - 1]
    // The summary goes last unless the log already ends with it.
    return last?.message === error.message
      ? lines
      : [...lines, { time: now.toISOString(), message: error.message, level: 'error' }]
  }
  return [{ time: now.toISOString(), message: error.message, level: 'error' }]
}

/**
 * Parity F-643: the Log node stores its Warning level as `warn` (kept, for
 * import compatibility) and the run passes it through, but the log panel
 * only knows `warning`, so a warning showed untinted with a success tick.
 */
export function withPanelLevel<T extends { level: string }>(entry: T): T {
  return entry.level === 'warn' ? { ...entry, level: 'warning' } : entry
}

/** The log panel's one line for a run that finishes in the background (F-207). */
export function startedInBackgroundLog(now: Date = new Date()): RunLogLine[] {
  return [{ time: now.toISOString(), message: FLOW_STARTED_IN_BACKGROUND, level: 'info' }]
}
