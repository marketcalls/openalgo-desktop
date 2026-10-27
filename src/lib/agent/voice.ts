/**
 * The spoken surface's types, as the chat page uses them.
 *
 * Desktop: a placeholder until the voice part lands. The voice controller
 * (`createVoiceController`, WebRTC) comes with it, verbatim from the web's
 * `lib/agent/voice.ts`; the types below are the web's, unchanged, so the
 * page compiles against the same shapes.
 */

export type VoiceState = 'idle' | 'connecting' | 'listening' | 'thinking' | 'speaking' | 'error'

/** What the button renders: one state and, when there is one, one reason. */
export interface VoiceStatus {
  state: VoiceState
  /** Plain language, safe to show. Null unless `state` is `error`. */
  error: string | null
}

/** One side of one spoken exchange, closed by a pause rather than an event. */
export interface VoiceTranscriptLine {
  /** Stable while the line is open, which is what keeps React keys honest. */
  id: string
  /** `trader` is the microphone, `agent` is what was spoken back. */
  role: 'trader' | 'agent'
  text: string
  /** False while more deltas may still arrive on this line. */
  final: boolean
}

/** The connection, its state machine, and the delegation loop. */
export interface VoiceController {
  /** Open the microphone, mint the session and connect. Idempotent. */
  start: () => Promise<void>
  /** Close everything and release the microphone. Safe to call when idle. */
  stop: () => void
  /** The current status, for a first render before anything has changed. */
  status: () => VoiceStatus
  /** The transcript so far, newest last. */
  transcript: () => VoiceTranscriptLine[]
  /** Subscribe to status changes. Returns its own unsubscribe. */
  onStatus: (listener: (status: VoiceStatus) => void) => () => void
  /** Subscribe to transcript changes. Returns its own unsubscribe. */
  onTranscript: (listener: (lines: VoiceTranscriptLine[]) => void) => () => void
  /** The conversation spoken turns are being stored against, once there is one. */
  conversationId: () => number | string | null
  /**
   * Tell the trader a tool is running, so a slow lookup is not silence.
   *
   * Called by the page while the turn it is running reports tool activity. A
   * no-op unless a spoken turn is waiting on an answer.
   */
  sayWorking: (toolName: string) => void
  /**
   * Listen for a spoken approval of one paused run.
   *
   * Each finalised thing the trader says while this is set is offered to the
   * server, which decides. `approve` runs only on a server-side yes.
   */
  awaitApproval: (runId: string, approve: () => void) => void
  /** Stop listening, because the run was answered on screen or abandoned. */
  cancelApproval: () => void
}
