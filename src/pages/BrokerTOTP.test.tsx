import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { MemoryRouter, Route, Routes } from 'react-router'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useAuthStore } from '@/stores/authStore'
import { useBrokerStore } from '@/stores/brokerStore'
import { useThemeStore } from '@/stores/themeStore'
import BrokerTOTP from './BrokerTOTP'

afterEach(() => {
  vi.unstubAllGlobals()
})

function renderAt(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/broker/:broker/totp" element={<BrokerTOTP />} />
      </Routes>
    </MemoryRouter>
  )
}

describe('BrokerTOTP login OTP', () => {
  it('offers Send OTP when the server did not send one, and posts it with the CSRF token', async () => {
    const calls: { url: string; body?: FormData }[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push({ url, body: init?.body as FormData | undefined })
        return {
          ok: true,
          json: async () =>
            url === '/auth/csrf-token'
              ? { csrf_token: 'tok' }
              : { status: 'success', message: 'OTP has been resent successfully' },
        }
      })
    )
    renderAt('/broker/nubra/totp?otp=send')
    expect(screen.getByText(/No OTP has been sent yet/)).toBeInTheDocument()
    // The field hint claims an OTP was sent; it waits until one was.
    expect(screen.queryByText(/For a new code/)).not.toBeInTheDocument()
    expect(calls).toHaveLength(0)

    await userEvent.click(screen.getByRole('button', { name: 'Send OTP' }))

    await waitFor(() =>
      expect(
        screen.getByText('An OTP has been sent to your registered mobile number.')
      ).toBeInTheDocument()
    )
    expect(screen.queryByRole('button', { name: 'Send OTP' })).not.toBeInTheDocument()
    expect(screen.getByText(/For a new code/)).toBeInTheDocument()
    const post = calls.find((c) => c.url === '/nubra/callback')
    expect(post?.body?.get('action')).toBe('resend')
    expect(post?.body?.get('csrf_token')).toBe('tok')
  })

  it('shows the reason when the OTP could not be sent, and keeps Send OTP', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) => ({
        ok: url === '/auth/csrf-token',
        json: async () =>
          url === '/auth/csrf-token'
            ? { csrf_token: 'tok' }
            : {
                status: 'error',
                message: 'Too many login attempts. Please wait a minute and try again.',
              },
      }))
    )
    renderAt('/broker/definedge/totp?otp=send')

    await userEvent.click(screen.getByRole('button', { name: 'Send OTP' }))

    await waitFor(() =>
      expect(
        screen.getByText('Too many login attempts. Please wait a minute and try again.')
      ).toBeInTheDocument()
    )
    expect(screen.getByRole('button', { name: 'Send OTP' })).toBeInTheDocument()
  })

  it('shows no Send OTP action when the OTP was sent as the page opened', () => {
    renderAt('/broker/nubra/totp')
    expect(screen.queryByRole('button', { name: 'Send OTP' })).not.toBeInTheDocument()
    expect(screen.getByText(/For a new code/)).toBeInTheDocument()
  })

  it('shows no Send OTP action for brokers that do not text a login OTP', () => {
    renderAt('/broker/angel/totp?otp=send')
    expect(screen.queryByRole('button', { name: 'Send OTP' })).not.toBeInTheDocument()
  })
})

// Desktop: a small focused card per broker (OpenBull's broker login): only
// that broker's fields, labelled its way, and a way back to Select Broker.
describe('BrokerTOTP login card', () => {
  it("shows only the broker's own fields and a link back to Select Broker", () => {
    renderAt('/broker/angel/totp')
    expect(
      screen.getByRole('heading', { level: 1, name: 'Login with Angel One' })
    ).toBeInTheDocument()
    expect(screen.getByLabelText('Client ID')).toBeInTheDocument()
    expect(screen.getByLabelText('PIN')).toBeInTheDocument()
    expect(screen.getByLabelText('TOTP')).toBeInTheDocument()
    expect(screen.queryByLabelText('Password')).toBeNull()
    expect(screen.getByRole('link', { name: /back to select broker/i })).toHaveAttribute(
      'href',
      '/broker'
    )
    expect(screen.getByRole('button', { name: 'Log in to Angel One' })).toBeInTheDocument()
  })

  it('asks Groww only for an optional TOTP or access token, and signs in with neither', async () => {
    const posted: FormData[] = []
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string, init?: RequestInit) => {
        if (init?.body instanceof FormData) posted.push(init.body)
        return {
          ok: true,
          json: async () =>
            url === '/auth/csrf-token' ? { csrf_token: 'tok' } : { status: 'success' },
        }
      })
    )
    renderAt('/broker/groww/totp')
    expect(screen.getByLabelText(/TOTP/)).not.toBeRequired()
    expect(screen.getByLabelText(/Access Token/)).not.toBeRequired()
    // No User ID or Password: Groww reads `password` as a pasted token.
    expect(screen.queryByLabelText(/User ID/)).toBeNull()
    await userEvent.click(screen.getByRole('button', { name: 'Log in to Groww' }))
    await waitFor(() => expect(posted).toHaveLength(1))
    expect([...posted[0].keys()]).toEqual(['csrf_token'])
  })
})

/** The API key the dashboard found in memory when it opened. */
let keyAtDashboard: string | null = null
function Dashboard() {
  keyAtDashboard = useAuthStore.getState().apiKey
  return <div>dashboard</div>
}

// Web 9d83c5de1 (#2423): a login on this card moves on without a page reload,
// so it reads the session itself and the dashboard opens with the API key.
describe('BrokerTOTP after a login', () => {
  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('has the API key in memory before the dashboard opens', async () => {
    useAuthStore.setState({
      user: null,
      apiKey: null,
      isAuthenticated: false,
      apiKeyChecked: false,
    })
    vi.spyOn(useBrokerStore.getState(), 'fetchCapabilities').mockResolvedValue()
    vi.spyOn(useThemeStore.getState(), 'syncAppMode').mockResolvedValue()
    const session = { status: 'success', authenticated: true, logged_in: true, user: 'trader' }
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) => ({
        ok: true,
        json: async () =>
          url === '/auth/csrf-token'
            ? { csrf_token: 'tok' }
            : url === '/auth/session-status'
              ? { ...session, broker: 'groww', api_key: 'KEY-1' }
              : { status: 'success' },
      }))
    )
    render(
      <MemoryRouter initialEntries={['/broker/groww/totp']}>
        <Routes>
          <Route path="/broker/:broker/totp" element={<BrokerTOTP />} />
          <Route path="/dashboard" element={<Dashboard />} />
        </Routes>
      </MemoryRouter>
    )
    await userEvent.click(screen.getByRole('button', { name: 'Log in to Groww' }))
    await screen.findByText('dashboard')
    expect(keyAtDashboard).toBe('KEY-1')
  })
})
