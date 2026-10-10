/**
 * useWebSocketTester (the Playground's WebSocket panel), copied from the web.
 *
 * PG-02: the log, the screen and the export never hold the API key, while the
 * socket still sends it. PG-01: a connect that fails, or is abandoned, before
 * its socket exists never leaves the Connect button latched.
 */

import { act, renderHook, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { redactRaw, redactSecrets, useWebSocketTester } from './useWebSocketTester'

const SENTINEL = 'sentinel-api-key-0123456789abcdef'

class FakeSocket {
  static CONNECTING = 0
  static OPEN = 1
  static CLOSING = 2
  static CLOSED = 3
  static instances: FakeSocket[] = []

  readyState = FakeSocket.CONNECTING
  onopen: (() => void) | null = null
  onclose: ((event: { wasClean: boolean; code: number }) => void) | null = null
  onerror: (() => void) | null = null
  onmessage: ((event: MessageEvent) => void) | null = null
  send = vi.fn()
  close = vi.fn(() => {
    this.readyState = FakeSocket.CLOSED
    this.onclose?.({ wasClean: true, code: 1000 })
  })

  constructor(public url: string) {
    FakeSocket.instances.push(this)
  }

  open() {
    this.readyState = FakeSocket.OPEN
    this.onopen?.()
  }

  receive(data: string) {
    this.onmessage?.({ data } as MessageEvent)
  }
}

function json(body: unknown) {
  return Promise.resolve({ json: () => Promise.resolve(body) } as Response)
}

/** Answers for each URL; a test replaces one to make it fail or hang. */
let routes: Record<string, () => Promise<Response>>

beforeEach(() => {
  FakeSocket.instances = []
  routes = {
    '/auth/csrf-token': () => json({ csrf_token: 'csrf' }),
    '/api/websocket/config': () =>
      json({ status: 'success', websocket_url: 'ws://127.0.0.1:8765' }),
    '/api/websocket/apikey': () => json({ status: 'success', api_key: SENTINEL }),
  }
  vi.stubGlobal('WebSocket', FakeSocket)
  vi.stubGlobal(
    'fetch',
    vi.fn((url: string) => {
      const route = routes[url]
      if (!route) throw new Error(`unexpected fetch ${url}`)
      return route()
    })
  )
})

afterEach(() => {
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('redaction (PG-02)', () => {
  it('replaces credential fields at any depth and in any letter case', () => {
    const out = JSON.stringify(
      redactSecrets({
        action: 'authenticate',
        API_KEY: SENTINEL,
        list: [{ nested: { apikey: SENTINEL, Password: SENTINEL, client_secret: SENTINEL } }],
        auth: { access_token: SENTINEL, symbol: 'SBIN' },
      })
    )
    expect(out).not.toContain(SENTINEL)
    expect(out).toContain('SBIN')
  })

  it('leaves a frame without credentials byte for byte, and redacts text that is not JSON', () => {
    const frame = '{"type":"market_data","symbol":"SBIN","data":{"ltp":812.5}}'
    expect(redactRaw(frame)).toBe(frame)
    expect(redactRaw(`bad frame apikey=${SENTINEL}&x=1`)).not.toContain(SENTINEL)
    expect(redactRaw(`{"api_key": "${SENTINEL}", broken`)).not.toContain(SENTINEL)
  })
})

describe('useWebSocketTester', () => {
  it('sends the real key but never logs, shows or exports it (PG-02)', async () => {
    const { result } = renderHook(() => useWebSocketTester())
    await act(async () => {
      await result.current.connect()
    })
    const socket = FakeSocket.instances[0]
    await act(async () => {
      socket.open()
    })
    await waitFor(() => expect(socket.send).toHaveBeenCalledTimes(1))
    expect(socket.send.mock.calls[0][0]).toContain(SENTINEL)

    // A manual frame with the key at the top and a token nested inside.
    act(() => {
      result.current.sendMessage(
        JSON.stringify({ action: 'subscribe', apikey: SENTINEL, meta: { Token: SENTINEL } })
      )
    })
    expect(socket.send.mock.calls[1][0]).toContain(SENTINEL)
    act(() => {
      socket.receive(`echo api_key=${SENTINEL}`)
    })

    expect(result.current.messages.length).toBeGreaterThanOrEqual(4)
    expect(JSON.stringify(result.current.messages)).not.toContain(SENTINEL)

    const blobs: Blob[] = []
    vi.spyOn(URL, 'createObjectURL').mockImplementation((blob) => {
      blobs.push(blob as Blob)
      return 'blob:export'
    })
    vi.spyOn(URL, 'revokeObjectURL').mockImplementation(() => {})
    vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {})
    act(() => {
      result.current.exportMessages()
    })
    expect(blobs).toHaveLength(1)
    // jsdom's Blob has no text(); read it the way a browser page would.
    const exported = await new Promise<string>((resolve) => {
      const reader = new FileReader()
      reader.onload = () => resolve(String(reader.result))
      reader.readAsText(blobs[0])
    })
    expect(exported).not.toContain(SENTINEL)
    expect(exported).toContain('[redacted]')
  })

  it('connects again after a failure before the socket exists (PG-01)', async () => {
    routes['/auth/csrf-token'] = () => Promise.reject(new Error('offline'))
    const { result } = renderHook(() => useWebSocketTester())
    await act(async () => {
      await result.current.connect()
    })
    expect(result.current.error).toContain('Connection failed')
    expect(result.current.isConnecting).toBe(false)
    expect(FakeSocket.instances).toHaveLength(0)

    routes['/auth/csrf-token'] = () => json({ csrf_token: 'csrf' })
    await act(async () => {
      await result.current.connect()
    })
    expect(FakeSocket.instances).toHaveLength(1)
  })

  it('ignores an attempt abandoned by Disconnect when it completes late (PG-01)', async () => {
    let finish: (r: Response) => void = () => {}
    routes['/api/websocket/config'] = () =>
      new Promise<Response>((resolve) => {
        finish = resolve
      })
    const { result } = renderHook(() => useWebSocketTester())
    let first: Promise<void> = Promise.resolve()
    act(() => {
      first = result.current.connect()
    })
    await waitFor(() =>
      expect(fetch).toHaveBeenCalledWith('/api/websocket/config', expect.anything())
    )
    act(() => {
      result.current.disconnect()
    })
    expect(result.current.isConnecting).toBe(false)

    // The abandoned attempt's answer arrives: it must not open a socket.
    await act(async () => {
      finish({
        json: () => Promise.resolve({ status: 'success', websocket_url: 'ws://127.0.0.1:8765' }),
      } as Response)
      await first
    })
    expect(FakeSocket.instances).toHaveLength(0)

    // And Connect works again at once.
    routes['/api/websocket/config'] = () =>
      json({ status: 'success', websocket_url: 'ws://127.0.0.1:8765' })
    await act(async () => {
      await result.current.connect()
    })
    expect(FakeSocket.instances).toHaveLength(1)
  })
})
