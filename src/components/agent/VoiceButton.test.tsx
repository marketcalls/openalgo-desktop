/**
 * The microphone in the composer: absent while voice is off or has no key
 * (as on the web), and, in an app window that cannot do voice, shown
 * disabled with the reason (decision 13).
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { VoiceConfig, VoiceConfigResponse } from '@/api/agent'
import { VoiceButton } from './VoiceButton'

const api = vi.hoisted(() => ({ getVoiceConfig: vi.fn() }))
vi.mock('@/api/agent', async (original) => ({
  ...(await original<typeof import('@/api/agent')>()),
  getVoiceConfig: api.getVoiceConfig,
}))
const reason = vi.hoisted(() => ({ voiceUnavailableReason: vi.fn((): string | null => null) }))
vi.mock('@/lib/agent/voiceDesktop', async (original) => ({
  ...(await original<typeof import('@/lib/agent/voiceDesktop')>()),
  voiceUnavailableReason: reason.voiceUnavailableReason,
}))

function config(over: Partial<VoiceConfig> = {}): VoiceConfigResponse {
  return {
    data: {
      voice_enabled: true,
      voice_model: 'gpt-live-1',
      voice_speaker: 'marin',
      voice_agent_name: 'Vega',
      voice_trading_enabled: false,
      voice_confirm_window_seconds: 30,
      voice_idle_timeout_seconds: 180,
      trading_enabled_master: false,
      trading_effective: false,
      speakers: ['marin'],
      key: { has_value: true, fingerprint: '...abcd sha256:0123456789ab', last_used_at: null },
      ...over,
    },
    defaults: null,
  }
}

function show() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(
    <QueryClientProvider client={client}>
      <VoiceButton />
    </QueryClientProvider>
  )
}

describe('VoiceButton', () => {
  beforeEach(() => {
    reason.voiceUnavailableReason.mockReturnValue(null)
  })
  afterEach(() => {
    vi.clearAllMocks()
  })

  it('offers the microphone when voice is on and a key is stored', async () => {
    api.getVoiceConfig.mockResolvedValue(config())
    show()
    const button = await screen.findByRole('button', { name: 'Talk to Vega' })
    expect(button).toBeEnabled()
  })

  it('renders nothing while voice is off or no key is stored', async () => {
    for (const over of [{ voice_enabled: false }, { key: { has_value: false } }] as const) {
      api.getVoiceConfig.mockResolvedValue(config(over as Partial<VoiceConfig>))
      const { container, unmount } = show()
      await vi.waitFor(() => expect(api.getVoiceConfig).toHaveBeenCalled())
      expect(container).toBeEmptyDOMElement()
      unmount()
    }
  })

  it('says why voice cannot work in this window instead of hiding', async () => {
    const why = 'Voice needs WebRTC, which the web view on this Linux system does not provide.'
    reason.voiceUnavailableReason.mockReturnValue(why)
    api.getVoiceConfig.mockResolvedValue(config())
    show()
    const button = await screen.findByRole('button', { name: 'Talk to Vega' })
    expect(button).toBeDisabled()
    expect(button).toHaveAccessibleDescription(why)
    expect(screen.getByText(why)).toBeVisible()
  })
})
