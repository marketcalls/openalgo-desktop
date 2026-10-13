import { act, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter, useNavigate } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useAuthStore } from '@/stores/authStore'
import { AuthSync, beforeBroker } from './AuthSync'

/** Answers for /auth/session-status, in order; other URLs get a bare success. */
let statusAnswers: Array<Record<string, unknown>> = []
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

function renderAt(path: string) {
  render(
    <MemoryRouter initialEntries={[path]}>
      <AuthSync>
        <Navigator />
        <div>page</div>
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
    useAuthStore.setState({ user: null, apiKey: null, isAuthenticated: false })
    vi.stubGlobal(
      'fetch',
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input)
        if (url.includes('/auth/session-status')) {
          statusCalls += 1
          return json(statusAnswers.shift() ?? NOT_SIGNED_IN)
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
