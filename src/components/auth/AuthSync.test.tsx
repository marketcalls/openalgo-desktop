import { act, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, useNavigate } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useAuthStore } from '@/stores/authStore'
import { useBrokerStore } from '@/stores/brokerStore'
import { useThemeStore } from '@/stores/themeStore'
import { AuthSync, beforeBroker, syncBrokerSession } from './AuthSync'

/**
 * Answers for /auth/session-status, in order; other URLs get a bare success.
 * A promise answers when it settles; one that rejects is a server that could
 * not be reached.
 */
let statusAnswers: Array<Record<string, unknown> | Promise<Record<string, unknown>>> = []
let statusCalls = 0

function json(body: unknown) {
  return Promise.resolve(
    new Response(JSON.stringify(body), {
      status: 200,
      headers: { 'Content-Type': 'application/json' },
    })
  )
}

let navigateTo: (path: string) => void = () => {}
function Navigator() {
  navigateTo = useNavigate()
  return null
}

/** The key each render of the page saw, as the books, quotes and order pages read it. */
let keysSeen: Array<string | null> = []
function Page() {
  keysSeen.push(useAuthStore((s) => s.apiKey))
  return <div>page</div>
}

/** An answer held until the test releases it. */
function held() {
  let release!: (body: Record<string, unknown>) => void
  const answer = new Promise<Record<string, unknown>>((resolve) => {
    release = resolve
  })
  return { answer, release }
}

function renderAt(path: string) {
  render(
    <MemoryRouter initialEntries={[path]}>
      <AuthSync>
        <Navigator />
        <Page />
      </AuthSync>
    </MemoryRouter>
  )
}

const NOT_SIGNED_IN = { status: 'success', authenticated: false, logged_in: false }
const SIGNED_IN_WITH_BROKER = {
  status: 'success',
  authenticated: true,
  logged_in: true,
  user: 'trader',
  broker: 'fyers',
  api_key: 'KEY-1',
}

describe('AuthSync', () => {
  beforeEach(() => {
    statusAnswers = []
    statusCalls = 0
    keysSeen = []
    useAuthStore.setState({
      user: null,
      apiKey: null,
      isAuthenticated: false,
      apiKeyChecked: false,
    })
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        if (url.includes('/auth/session-status')) {
          statusCalls += 1
          return Promise.resolve(statusAnswers.shift() ?? NOT_SIGNED_IN).then(json)
        }
        return json({ status: 'success', data: {} })
      })
    )
  })
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('loads the API key after a sign-in that did not reload the page', async () => {
    // The window opened before the trader signed in.
    statusAnswers.push(NOT_SIGNED_IN, SIGNED_IN_WITH_BROKER)
    renderAt('/login')
    await screen.findByText('page')
    expect(statusCalls).toBe(1)

    // Sign-in resumed the broker session and went to a page that needs the key.
    act(() => {
      useAuthStore.getState().login('trader', 'fyers')
      navigateTo('/scalping')
    })

    await waitFor(() => expect(useAuthStore.getState().apiKey).toBe('KEY-1'))
    expect(statusCalls).toBe(2)
  })

  it('does not check again on the broker pages, where no broker is expected yet', async () => {
    statusAnswers.push(NOT_SIGNED_IN)
    renderAt('/login')
    await screen.findByText('page')

    act(() => {
      useAuthStore.getState().login('trader', '')
      navigateTo('/broker')
    })

    // Give any effect a chance to run.
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20))
    })
    expect(statusCalls).toBe(1)
    expect(useAuthStore.getState().isAuthenticated).toBe(true)
  })

  // The key is kept in memory only, so after a sign-in it is not there until
  // the check answers. A page that reads it must not start without it.
  it('holds a page back until the API key is in memory', async () => {
    const status = held()
    statusAnswers.push(NOT_SIGNED_IN, status.answer)
    renderAt('/login')
    await screen.findByText('page')

    keysSeen = []
    act(() => {
      useAuthStore.getState().login('trader', 'fyers')
      navigateTo('/positions')
    })
    expect(screen.queryByText('page')).toBeNull()

    await act(async () => status.release(SIGNED_IN_WITH_BROKER))
    await screen.findByText('page')
    expect(keysSeen.length).toBeGreaterThan(0)
    expect(keysSeen.every((key) => key === 'KEY-1')).toBe(true)
  })

  it('lets the page through when the trader has no API key yet', async () => {
    statusAnswers.push(NOT_SIGNED_IN, { ...SIGNED_IN_WITH_BROKER, api_key: null })
    renderAt('/login')
    await screen.findByText('page')

    act(() => {
      useAuthStore.getState().login('trader', 'fyers')
      navigateTo('/positions')
    })
    await screen.findByText('page')
    expect(useAuthStore.getState().apiKey).toBeNull()

    // Not held back again on the next page; the check runs behind it.
    act(() => navigateTo('/orderbook'))
    expect(screen.getByText('page')).toBeInTheDocument()
  })

  it('lets the page through when the server cannot be asked', async () => {
    const offline = Promise.reject(new Error('offline'))
    offline.catch(() => {}) // Read by the stubbed fetch later, not unhandled.
    statusAnswers.push(NOT_SIGNED_IN, offline)
    renderAt('/login')
    await screen.findByText('page')

    act(() => {
      useAuthStore.getState().login('trader', 'fyers')
      navigateTo('/positions')
    })
    await screen.findByText('page')
    expect(useAuthStore.getState().isAuthenticated).toBe(true)
  })

  it('waits for the key again after a logout and a new sign-in', async () => {
    const status = held()
    statusAnswers.push(NOT_SIGNED_IN, { ...SIGNED_IN_WITH_BROKER, api_key: null }, status.answer)
    renderAt('/login')
    await screen.findByText('page')
    act(() => {
      useAuthStore.getState().login('trader', 'fyers')
      navigateTo('/positions')
    })
    await screen.findByText('page')

    act(() => {
      useAuthStore.getState().logout()
      navigateTo('/login')
    })
    keysSeen = []
    act(() => {
      useAuthStore.getState().login('trader', 'fyers')
      navigateTo('/holdings')
    })
    expect(screen.queryByText('page')).toBeNull()

    await act(async () => status.release(SIGNED_IN_WITH_BROKER))
    await screen.findByText('page')
    expect(keysSeen.every((key) => key === 'KEY-1')).toBe(true)
  })

  // PROD-SEC-01: a reload starts from what storage kept (signed in, no key).
  // The page waits for the server's answer, and the key it gets is the one
  // that answer carries.
  it('after a reload, holds the page until the server has checked the session', async () => {
    useAuthStore.setState({
      user: { username: 'trader', broker: 'fyers', isLoggedIn: true, loginTime: null },
      isAuthenticated: true,
    })
    const status = held()
    statusAnswers.push(status.answer)
    renderAt('/positions')
    expect(screen.queryByText('page')).toBeNull()
    expect(keysSeen).toEqual([])

    await act(async () => status.release(SIGNED_IN_WITH_BROKER))
    await screen.findByText('page')
    expect(statusCalls).toBe(1)
    expect(keysSeen.length).toBeGreaterThan(0)
    expect(keysSeen.every((key) => key === 'KEY-1')).toBe(true)
  })

  // The session ended while the window was closed or in another window (a
  // logout there, a password change, the app restarting): the reload's check
  // says so, and the window is signed out with no key in memory.
  it('after a reload, signs the window out when the session ended elsewhere', async () => {
    useAuthStore.setState({
      user: { username: 'trader', broker: 'fyers', isLoggedIn: true, loginTime: null },
      isAuthenticated: true,
    })
    statusAnswers.push(NOT_SIGNED_IN)
    renderAt('/positions')

    await screen.findByText('page')
    expect(useAuthStore.getState()).toMatchObject({
      apiKey: null,
      isAuthenticated: false,
      user: null,
    })
    expect(keysSeen.every((key) => key === null)).toBe(true)
  })

  it('stops checking once the key is there', async () => {
    statusAnswers.push(SIGNED_IN_WITH_BROKER)
    renderAt('/dashboard')
    await waitFor(() => expect(useAuthStore.getState().apiKey).toBe('KEY-1'))
    const calls = statusCalls
    act(() => navigateTo('/scalping'))
    act(() => navigateTo('/positions'))
    await act(async () => {
      await new Promise((r) => setTimeout(r, 20))
    })
    expect(statusCalls).toBe(calls)
  })
})

describe('beforeBroker', () => {
  it('names the public and broker sign-in pages, and nothing else', () => {
    for (const p of [
      '/',
      '/login',
      '/setup',
      '/faq',
      '/broker',
      '/broker/config',
      '/broker/zerodha/totp',
      '/zerodha/auth',
    ]) {
      expect(beforeBroker(p), p).toBe(true)
    }
    for (const p of ['/dashboard', '/scalping', '/positions', '/orderbook', '/trading']) {
      expect(beforeBroker(p), p).toBe(false)
    }
  })
})

/**
 * Issue 2417 (web 9d83c5de1): a broker login that completes inside the app
 * (the TOTP and Samco pages) moves on without a page reload, so AuthSync never
 * ran for it and the API key stayed unset; every order ticket's button was
 * disabled until the trader reloaded. syncBrokerSession re-reads the session for them.
 */
describe('syncBrokerSession', () => {
  function respondWith(body: unknown, ok = true) {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({ ok, json: async () => body }) as Response)
    )
  }

  beforeEach(() => {
    useAuthStore.setState({
      user: null,
      apiKey: null,
      isAuthenticated: false,
      apiKeyChecked: false,
    })
    vi.spyOn(useBrokerStore.getState(), 'fetchCapabilities').mockResolvedValue()
    vi.spyOn(useThemeStore.getState(), 'syncAppMode').mockResolvedValue()
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.restoreAllMocks()
  })

  it('loads the API key, capabilities and app mode after an in-app login', async () => {
    respondWith({
      status: 'success',
      logged_in: true,
      user: 'trader',
      broker: 'angel',
      api_key: 'key-123',
      active_sessions: 1,
    })
    await expect(syncBrokerSession()).resolves.toBe(true)
    expect(useAuthStore.getState().apiKey).toBe('key-123')
    expect(useAuthStore.getState().user?.broker).toBe('angel')
    expect(useBrokerStore.getState().fetchCapabilities).toHaveBeenCalled()
    expect(useThemeStore.getState().syncAppMode).toHaveBeenCalled()
  })

  // Desktop: the page the login goes to is not held back waiting for a key
  // the server was just asked for (AuthSync's apiKeyChecked).
  it('lets a page that waits for the key go ahead', async () => {
    respondWith({ status: 'success', logged_in: true, user: 'trader', broker: 'angel' })
    await expect(syncBrokerSession()).resolves.toBe(true)
    expect(useAuthStore.getState()).toMatchObject({ apiKey: null, apiKeyChecked: true })
  })

  it('changes nothing when the server reports no broker session', async () => {
    respondWith({ status: 'success', authenticated: true, logged_in: false })
    await expect(syncBrokerSession()).resolves.toBe(false)
    expect(useAuthStore.getState().apiKey).toBeNull()
  })

  it('changes nothing when the request fails', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('offline')
      })
    )
    await expect(syncBrokerSession()).resolves.toBe(false)
    expect(useAuthStore.getState().apiKey).toBeNull()
  })
})
