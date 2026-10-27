/**
 * The voice panel says what the server does (D-034), names no approval word
 * (D-043), and sends only what moved.
 */

import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import type { VoiceConfig, VoiceConfigResponse, VoiceDefaults } from '@/api/agent'
import { VoicePanel } from './VoicePanel'

const api = vi.hoisted(() => ({ getVoiceConfig: vi.fn(), updateVoiceConfig: vi.fn() }))
vi.mock('@/api/agent', async (original) => ({
  ...(await original<typeof import('@/api/agent')>()),
  getVoiceConfig: api.getVoiceConfig,
  updateVoiceConfig: api.updateVoiceConfig,
}))

const defaults: VoiceDefaults = {
  voice_enabled: false,
  voice_model: 'gpt-live-1',
  voice_speaker: 'marin',
  voice_agent_name: 'Vega',
  voice_trading_enabled: false,
  voice_confirm_window_seconds: 30,
  voice_idle_timeout_seconds: 180,
}

const data: VoiceConfig = {
  ...defaults,
  voice_enabled: true,
  trading_enabled_master: true,
  trading_effective: false,
  speakers: ['marin', 'cedar'],
  key: { has_value: false, fingerprint: null, last_used_at: null },
}

function show() {
  api.getVoiceConfig.mockResolvedValue({ data, defaults } satisfies VoiceConfigResponse)
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  render(
    <QueryClientProvider client={client}>
      <VoicePanel />
    </QueryClientProvider>
  )
}

describe('VoicePanel', () => {
  it('describes the approval window as the server keeps it', async () => {
    show()
    const text = (await screen.findByText(/A plain\s+yes approves it once/)).textContent ?? ''
    expect(text).toMatch(/anything else is not an answer and leaves\s+it open/)
    expect(text).not.toMatch(/first thing said/)
    expect(document.body.textContent).not.toMatch(/approval word|order phrase|wake phrase/i)
  })

  it('saves only the field that moved', async () => {
    api.updateVoiceConfig.mockResolvedValue({ ...data, voice_agent_name: 'Tara' })
    show()
    const name = await screen.findByLabelText('Name')
    fireEvent.change(name, { target: { value: ' Tara ' } })
    fireEvent.click(screen.getByRole('button', { name: 'Save settings' }))
    await waitFor(() =>
      expect(api.updateVoiceConfig).toHaveBeenCalledWith({ voice_agent_name: 'Tara' })
    )
  })
})
