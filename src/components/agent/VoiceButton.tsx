/**
 * The microphone button in the composer.
 *
 * Desktop: a placeholder until the voice part lands with the web's
 * `components/agent/VoiceButton.tsx`. It renders nothing, which is what the
 * web's button renders while voice is not configured, so the chat page
 * behaves as the web's does with voice off.
 */

import type { VoiceController, VoiceTranscriptLine } from '@/lib/agent/voice'

export interface VoiceButtonProps {
  /** The model a spoken turn runs on, matching the composer's own picker. */
  modelId?: number | null
  /** Whether this surface asks for order tools. The backend still decides. */
  tradingEnabled?: boolean
  /** Blocks starting a session, for a surface that is not ready to run a turn. */
  disabled?: boolean
  /** Runs one spoken turn on the page's own stream and returns what to say. */
  ask?: (question: string) => Promise<string>
  /** Receives the controller so the page can report tool activity into it. */
  onController?: (controller: VoiceController | null) => void
  /** Every finalised spoken line, for the page to record. */
  onSpokenLine?: (role: 'trader' | 'agent', text: string) => void
  /** The live transcript, for the page to render in the thread. */
  onTranscript?: (lines: VoiceTranscriptLine[]) => void
  className?: string
}

export function VoiceButton(_props: VoiceButtonProps) {
  return null
}
