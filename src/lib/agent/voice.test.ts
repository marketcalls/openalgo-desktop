/**
 * The voice controller: what reaches the speech model and how a session
 * ends. The `speakable` cases are the web's server-side ones
 * (`test/test_agent_voice.py`, TestWhatReachesTheDataChannel): the desktop
 * keeps one copy, the one on the path to the data channel (D-035).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createVoiceController, speakable } from './voice'

vi.mock('@/api/client', () => ({ fetchCSRFToken: async () => 'csrf' }))

const BUDGET = 480

describe('speakable', () => {
  it('strips markdown rather than reading it out', () => {
    const spoken = speakable('**Bank Nifty** is at `52,000` and _holding_.')
    expect(spoken).not.toMatch(/[*`_]/)
    expect(spoken).toContain('Bank Nifty')
    expect(spoken).toContain('52,000')
  })

  it('drops heading and bullet punctuation', () => {
    const spoken = speakable('# Summary\n- Nifty is up\n- Bank Nifty is flat')
    expect(spoken).not.toContain('#')
    expect(spoken.trimStart().startsWith('-')).toBe(false)
    expect(spoken).toContain('Nifty is up')
  })

  it('speaks a link as its text', () => {
    expect(speakable('See [the chain](https://example.com/x)')).toBe('See the chain')
  })

  it('drops a table and fenced code', () => {
    expect(
      speakable(
        'Here is the chain.\n| Strike | Call | Put |\n| --- | --- | --- |\n| 52000 | 120 | 95 |\nIt is balanced.'
      )
    ).toBe('Here is the chain. It is balanced.')
    expect(
      speakable(
        'Here is the script.\n```python\nfor i in range(10):\n    print(i)\n```\nThat is all.'
      )
    ).toBe('Here is the script. That is all.')
  })

  it('gives nothing for nothing and leaves a short answer alone', () => {
    expect(speakable('')).toBe('')
    expect(speakable('   \n\n  ')).toBe('')
    expect(speakable('  Nifty is at twenty three thousand four hundred.  ')).toBe(
      'Nifty is at twenty three thousand four hundred.'
    )
  })

  it('cuts a long answer at a sentence inside the budget', () => {
    const spoken = speakable('Bank Nifty is holding up. '.repeat(40))
    expect(spoken.length).toBeLessThanOrEqual(BUDGET)
    expect(spoken.endsWith('.')).toBe(true)
  })

  // D-035: the web's client added the ellipsis on top of a full budget.
  it('keeps the ellipsis inside the budget', () => {
    const words = speakable('word '.repeat(200))
    expect(words.endsWith('...')).toBe(true)
    expect(words.length).toBeLessThanOrEqual(BUDGET)
    const run = speakable('x'.repeat(600))
    expect(run.length).toBeLessThanOrEqual(BUDGET)
    expect(run).toBe(`${'x'.repeat(BUDGET - 3)}...`)
  })
})

/** Just enough WebRTC for a session to open: the channel is the test's. */
class FakeChannel extends EventTarget {
  readyState = 'open'
  send = vi.fn()
  close() {}
}

let channel: FakeChannel | null = null

class FakePeer extends EventTarget {
  iceGatheringState = 'complete'
  connectionState = 'new'
  localDescription = { sdp: 'v=0\r\n' }
  addTrack() {}
  createDataChannel() {
    channel = new FakeChannel()
    return channel
  }
  async createOffer() {
    return { type: 'offer', sdp: 'v=0\r\n' }
  }
  async setLocalDescription() {}
  async setRemoteDescription() {}
  close() {}
}

describe('a session that nobody uses', () => {
  beforeEach(() => {
    channel = null
    vi.stubGlobal('RTCPeerConnection', FakePeer)
    vi.stubGlobal(
      'Audio',
      class {
        autoplay = false
        srcObject: unknown = null
        play() {
          return Promise.resolve()
        }
        pause() {}
      }
    )
    vi.stubGlobal('isSecureContext', true)
    Object.defineProperty(navigator, 'mediaDevices', {
      configurable: true,
      value: { getUserMedia: async () => ({ getAudioTracks: () => [], getTracks: () => [] }) },
    })
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response('v=0\r\nanswer\r\n', {
            status: 200,
            headers: { 'content-type': 'application/sdp' },
          })
      )
    )
  })

  afterEach(() => {
    vi.useRealTimers()
    vi.unstubAllGlobals()
  })

  async function listening(idleTimeoutSeconds: number) {
    const controller = createVoiceController({ idleTimeoutSeconds })
    await controller.start()
    vi.useFakeTimers()
    channel?.dispatchEvent(new Event('open'))
    expect(controller.status().state).toBe('listening')
    return controller
  }

  // D-033: the web set the sentence on an idle state, which kept a message
  // only for errors, so it never reached the button.
  it('hangs up after the quiet timeout and says so until the next press', async () => {
    const controller = await listening(30)
    vi.advanceTimersByTime(30_000)
    expect(controller.status()).toEqual({
      state: 'idle',
      error: null,
      notice: 'Voice stopped after 30 seconds of quiet. Press the microphone to start again.',
    })
    vi.useRealTimers()
    const pressed = controller.start()
    expect(controller.status().notice).toBeNull()
    await pressed
  })

  it('names whole minutes as minutes', async () => {
    const controller = await listening(180)
    vi.advanceTimersByTime(180_000)
    expect(controller.status().notice).toBe(
      'Voice stopped after 3 minutes of quiet. Press the microphone to start again.'
    )
  })

  it('a stop pressed by the trader leaves no notice', async () => {
    const controller = await listening(30)
    controller.stop()
    expect(controller.status()).toEqual({ state: 'idle', error: null, notice: null })
  })
})
