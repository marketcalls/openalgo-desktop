/**
 * The voice controller's connection and delegation loop (the web's
 * `lib/agent/voice.ts`, which the web ships without tests): a spoken
 * question becomes a turn and its answer goes back to the speech model, an
 * approval is the server's decision, and every way a session ends releases
 * the microphone and says why.
 *
 * WebRTC, the microphone and the server are fakes; the page's own code runs.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createVoiceController, TURN_PAUSE_MS, type VoiceControllerOptions } from './voice'

vi.mock('@/api/client', () => ({ fetchCSRFToken: async () => 'csrf' }))
const shell = vi.hoisted(() => ({ isDesktopShell: vi.fn(() => false) }))
vi.mock('@/lib/desktop', () => shell)

type Sent = Record<string, unknown>

class FakeChannel extends EventTarget {
  readyState = 'open'
  sent: Sent[] = []
  send(data: string) {
    this.sent.push(JSON.parse(data))
  }
  close() {
    this.readyState = 'closed'
  }
}

let peers: FakePeer[] = []
let channel: FakeChannel

class FakePeer extends EventTarget {
  iceGatheringState = 'complete'
  connectionState = 'new'
  localDescription = { sdp: 'v=0\r\n' }
  closed = false
  remote: unknown = null
  constructor() {
    super()
    peers.push(this)
  }
  addTrack() {}
  createDataChannel(name: string) {
    expect(name).toBe('oai-events')
    channel = new FakeChannel()
    return channel
  }
  async createOffer() {
    return { type: 'offer', sdp: 'v=0\r\n' }
  }
  async setLocalDescription() {}
  async setRemoteDescription(answer: unknown) {
    this.remote = answer
  }
  close() {
    this.closed = true
  }
}

const track = { stop: vi.fn() }
const microphone = { getAudioTracks: () => [track], getTracks: () => [track] }

/** What the server answers, by route. */
let routes: Record<string, () => Response>
const calls: { path: string; body: string }[] = []

function sdpAnswer() {
  return new Response('v=0\nanswer', {
    status: 200,
    headers: { 'content-type': 'application/sdp' },
  })
}

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json' },
  })
}

function sse(frames: unknown[]) {
  return new Response(frames.map((f) => `data: ${JSON.stringify(f)}\n\n`).join(''), {
    status: 200,
    headers: { 'content-type': 'text/event-stream' },
  })
}

beforeEach(() => {
  peers = []
  calls.length = 0
  track.stop.mockClear()
  shell.isDesktopShell.mockReturnValue(false)
  routes = { '/agent/api/voice/session': sdpAnswer }
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
    value: { getUserMedia: vi.fn(async () => microphone) },
  })
  vi.stubGlobal(
    'fetch',
    vi.fn(async (url: string, init?: RequestInit) => {
      const path = new URL(url, 'http://127.0.0.1').pathname
      calls.push({ path, body: String(init?.body ?? '') })
      const route = routes[path]
      if (!route) throw new Error(`unexpected ${path}`)
      return route()
    })
  )
})

afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

/** A session through to the open channel, on fake timers from there on. */
async function live(options: VoiceControllerOptions = {}) {
  const controller = createVoiceController(options)
  await controller.start()
  vi.useFakeTimers()
  channel.dispatchEvent(new Event('open'))
  return controller
}

function emit(event: Sent) {
  channel.dispatchEvent(new MessageEvent('message', { data: JSON.stringify(event) }))
}

function spoken(type: string) {
  return channel.sent.filter((e) => e.type === type)
}

describe('connecting', () => {
  it('posts the complete offer and applies the answer, CRLF-terminated', async () => {
    const controller = await live()
    expect(controller.status().state).toBe('listening')
    expect(calls[0]).toEqual({ path: '/agent/api/voice/session', body: 'v=0\r\n' })
    expect(peers[0].remote).toEqual({ type: 'answer', sdp: 'v=0\r\nanswer\r\n' })
  })

  it('is one connection however often it is pressed', async () => {
    const controller = createVoiceController()
    await Promise.all([controller.start(), controller.start()])
    expect(peers).toHaveLength(1)
  })

  it('says what the server refused, and when the session was signed out', async () => {
    routes['/agent/api/voice/session'] = () =>
      json(
        { status: 'error', message: 'Voice is switched off. Turn it on in agent settings.' },
        400
      )
    const controller = createVoiceController()
    await controller.start()
    expect(controller.status()).toMatchObject({
      state: 'error',
      error: 'Voice is switched off. Turn it on in agent settings.',
    })
    expect(peers[0].closed).toBe(true)
    expect(track.stop).toHaveBeenCalled()
    routes['/agent/api/voice/session'] = () =>
      new Response('<!doctype html>', { status: 200, headers: { 'content-type': 'text/html' } })
    await controller.start()
    expect(controller.status().error).toBe(
      'You have been signed out. Reload the page and sign in again.'
    )
  })

  it.each([
    ['NotFoundError', 'No microphone was found.'],
    ['NotReadableError', 'Another app is using the microphone.'],
    ['NotAllowedError', 'The microphone was blocked. Allow it for this site in your browser'],
  ])('names a %s microphone refusal', async (name, says) => {
    const refusal = Object.assign(new Error('raw'), { name })
    vi.mocked(navigator.mediaDevices.getUserMedia).mockRejectedValueOnce(refusal)
    const controller = createVoiceController()
    await controller.start()
    expect(controller.status().state).toBe('error')
    expect(controller.status().error).toContain(says)
    expect(peers).toHaveLength(0)
  })

  it('says why a page cannot have a microphone at all', async () => {
    Object.defineProperty(navigator, 'mediaDevices', { configurable: true, value: undefined })
    const controller = createVoiceController()
    await controller.start()
    expect(controller.status().error).toBe(
      'This browser cannot use a microphone. Try Chrome, Edge or Firefox.'
    )
    vi.stubGlobal('isSecureContext', false)
    await controller.start()
    expect(controller.status().error).toContain('on the computer it runs on')
  })

  it('a stop during the permission prompt releases the microphone it gets', async () => {
    let grant: (stream: unknown) => void = () => {}
    vi.mocked(navigator.mediaDevices.getUserMedia).mockImplementationOnce(
      () => new Promise((resolve) => (grant = resolve as (stream: unknown) => void))
    )
    const controller = createVoiceController()
    const starting = controller.start()
    controller.stop()
    grant(microphone)
    await starting
    expect(track.stop).toHaveBeenCalled()
    expect(controller.status().state).toBe('idle')
    expect(peers).toHaveLength(0)
  })
})

describe('a spoken turn', () => {
  it('asks the page, speaks the answer and records each finished line', async () => {
    const ask = vi.fn(async () => '**Nifty** is up a little.')
    const onSpokenLine = vi.fn()
    const controller = await live({ ask, onSpokenLine })
    emit({ type: 'session.input_transcript.delta', delta: 'what is ' })
    emit({ type: 'session.input_transcript.delta', delta: 'nifty' })
    expect(controller.transcript()).toEqual([
      { id: 'trader-1', role: 'trader', text: 'what is nifty', final: false },
    ])
    emit({ type: 'session.delegation.created', delegation: { id: 'd1' } })
    expect(controller.status().state).toBe('thinking')
    await vi.advanceTimersByTimeAsync(0)
    expect(ask).toHaveBeenCalledWith('what is nifty')
    expect(spoken('session.commentary.append')).toMatchObject([
      { delegation_id: 'd1', content: 'Nifty is up a little.' },
    ])
    expect(controller.status().state).toBe('speaking')
    await vi.advanceTimersByTimeAsync(TURN_PAUSE_MS)
    expect(onSpokenLine).toHaveBeenCalledWith('trader', 'what is nifty')
    expect(controller.transcript()[0].final).toBe(true)
    emit({ type: 'session.output_transcript.delta', delta: 'Nifty is up.' })
    await vi.advanceTimersByTimeAsync(TURN_PAUSE_MS)
    expect(onSpokenLine).toHaveBeenCalledWith('agent', 'Nifty is up.')
    emit({ type: 'session.input_transcript.delta', transcript: 'and bank' })
    expect(controller.status().state).toBe('listening')
  })

  it('says so when nothing was heard, and when the turn failed', async () => {
    const ask = vi.fn(async () => {
      throw new Error('The model is busy.')
    })
    await live({ ask })
    emit({ type: 'session.delegation.created', delegation_id: 'd0' })
    expect(spoken('session.commentary.append')).toMatchObject([
      { delegation_id: 'd0', content: 'I did not catch that. Say it again.' },
    ])
    emit({ type: 'session.input_transcript.delta', delta: 'hello' })
    emit({ type: 'session.delegation.created', id: 'd1' })
    await vi.advanceTimersByTimeAsync(0)
    expect(spoken('session.commentary.append')[1]).toMatchObject({ content: 'The model is busy.' })
  })

  it('a newer question replaces the one in flight, and a slow tool is not silence', async () => {
    const answers: ((text: string) => void)[] = []
    const ask = vi.fn(() => new Promise<string>((resolve) => answers.push(resolve)))
    const controller = await live({ ask })
    controller.sayWorking('get_quote')
    expect(spoken('session.thinking.append')).toHaveLength(0)
    emit({ type: 'session.input_transcript.delta', delta: 'nifty' })
    emit({ type: 'session.delegation.created', delegation: { id: 'd1' } })
    await vi.advanceTimersByTimeAsync(0)
    controller.sayWorking('get_option_chain')
    expect(spoken('session.thinking.append')).toMatchObject([
      { delegation_id: 'd1', content: 'Still working. Looking up get option chain now.' },
    ])
    emit({ type: 'session.input_transcript.delta', delta: 'no, bank nifty' })
    emit({ type: 'session.delegation.created', delegation: { id: 'd2' } })
    await vi.advanceTimersByTimeAsync(0)
    answers[0]('Nifty is flat.')
    answers[1]('Bank Nifty is up.')
    await vi.advanceTimersByTimeAsync(0)
    expect(spoken('session.commentary.append')).toMatchObject([
      { delegation_id: 'd2', content: 'Bank Nifty is up.' },
    ])
  })

  it('runs the turn itself when the page does not, on the voice surface', async () => {
    routes['/agent/api/chat/stream'] = () =>
      sse([
        { type: 'start', run_id: 'r1', session_id: 's1', conversation_id: 7, user_message_id: 3 },
        { type: 'tool_start', id: 'c1', name: 'get_quote', args: {} },
        { type: 'token', delta: 'Infosys is at fifteen twenty.' },
        { type: 'done', reason: 'stop' },
      ])
    const onFrame = vi.fn()
    const controller = await live({ onFrame, modelId: 4, tradingEnabled: true })
    emit({ type: 'session.input_transcript.delta', delta: 'infosys' })
    emit({ type: 'session.delegation.created', delegation: { id: 'd1' } })
    await vi.advanceTimersByTimeAsync(0)
    await vi.waitFor(() => expect(spoken('session.commentary.append')).toHaveLength(1))
    const turn = JSON.parse(calls.find((c) => c.path === '/agent/api/chat/stream')?.body ?? '{}')
    expect(turn).toEqual({
      message: 'infosys',
      surface: 'voice',
      conversation_id: null,
      trading_enabled: true,
      model_id: 4,
    })
    expect(spoken('session.thinking.append')[0]).toMatchObject({
      content: 'Still working. Looking up get quote now.',
    })
    expect(spoken('session.commentary.append')[0]).toMatchObject({
      content: 'Infosys is at fifteen twenty.',
    })
    expect(onFrame).toHaveBeenCalledTimes(4)
    expect(controller.conversationId()).toBe(7)
  })

  it('keeps the last fifty lines', async () => {
    const controller = await live()
    for (let i = 0; i < 60; i += 1) {
      emit({ type: 'session.input_transcript.delta', delta: `line ${i}` })
      await vi.advanceTimersByTimeAsync(TURN_PAUSE_MS)
    }
    const lines = controller.transcript()
    expect(lines).toHaveLength(50)
    expect(lines[49].text).toBe('line 59')
  })
})

describe('a spoken approval', () => {
  it('is the server decision on what was said, once', async () => {
    let approved = false
    routes['/agent/api/voice/approve'] = () => json({ status: 'success', data: { approved } })
    const approve = vi.fn()
    const controller = await live()
    controller.awaitApproval('run-1', approve)
    emit({ type: 'session.input_transcript.delta', delta: 'what is the margin' })
    await vi.advanceTimersByTimeAsync(TURN_PAUSE_MS)
    expect(JSON.parse(calls.at(-1)?.body ?? '{}')).toEqual({
      run_id: 'run-1',
      transcript: 'what is the margin',
    })
    expect(approve).not.toHaveBeenCalled()
    approved = true
    emit({ type: 'session.input_transcript.delta', delta: 'yes' })
    await vi.advanceTimersByTimeAsync(TURN_PAUSE_MS)
    await vi.waitFor(() => expect(approve).toHaveBeenCalledTimes(1))
    const asked = calls.length
    emit({ type: 'session.input_transcript.delta', delta: 'yes' })
    await vi.advanceTimersByTimeAsync(TURN_PAUSE_MS)
    expect(calls.length).toBe(asked)
  })

  it('a failed check, a card answer or a hang-up is never an approval', async () => {
    routes['/agent/api/voice/approve'] = () => json({ message: 'no' }, 500)
    const approve = vi.fn()
    const controller = await live()
    controller.awaitApproval('run-1', approve)
    emit({ type: 'session.input_transcript.delta', delta: 'yes' })
    await vi.advanceTimersByTimeAsync(TURN_PAUSE_MS)
    controller.cancelApproval()
    routes['/agent/api/voice/approve'] = () => json({ data: { approved: true } })
    const asked = calls.length
    emit({ type: 'session.input_transcript.delta', delta: 'yes' })
    await vi.advanceTimersByTimeAsync(TURN_PAUSE_MS)
    expect(calls.length).toBe(asked)
    controller.awaitApproval('run-2', approve)
    controller.stop()
    expect(approve).not.toHaveBeenCalled()
  })
})

describe('a session that ends by itself', () => {
  it('says what the speech service reported, and releases everything', async () => {
    const controller = await live()
    emit({ type: 'error', error: { message: 'Session expired.' } })
    expect(controller.status()).toEqual({ state: 'error', error: 'Session expired.', notice: null })
    expect(channel.readyState).toBe('closed')
    expect(peers[0].closed).toBe(true)
    expect(track.stop).toHaveBeenCalled()
  })

  it('says the connection dropped', async () => {
    const controller = await live()
    peers[0].connectionState = 'failed'
    peers[0].dispatchEvent(new Event('connectionstatechange'))
    expect(controller.status().error).toBe(
      'The voice connection dropped. Press the microphone to start again.'
    )
  })

  it('ignores what it cannot read', async () => {
    const controller = await live()
    channel.dispatchEvent(new MessageEvent('message', { data: 'not json' }))
    emit({ type: 'session.unknown' })
    expect(controller.status().state).toBe('listening')
  })
})
