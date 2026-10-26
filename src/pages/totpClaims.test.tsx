import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { render, screen } from '@/test/test-utils'

// docs-34, docs-41: the TOTP tab and the setup page promised backup codes and
// a TOTP password reset. The desktop has neither: TOTP guards sign-in, and a
// forgotten password is replaced from the sign-in page.

const client = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }))
vi.mock('@/api/client', () => ({ webClient: client, fetchCSRFToken: vi.fn() }))

import Profile from './Profile'
import Setup from './Setup'

const UNTRUE = [/backup codes in a safe place/i, /reset your password/i, /password recovery/i]

beforeEach(() => {
  client.get.mockImplementation(async (url: string) => {
    if (url === '/auth/profile-data') {
      return {
        data: {
          status: 'success',
          data: { username: 'trader', qr_code: 'iVBORw0KGgo=', totp_secret: 'JBSWY3DPEHPK3PXP' },
        },
      }
    }
    if (url === '/api/broker/credentials') {
      return {
        data: {
          status: 'success',
          data: {
            current_broker: '',
            valid_brokers: [],
            ngrok_allow: false,
            host_server: 'http://127.0.0.1:5000',
            websocket_url: 'ws://127.0.0.1:8765',
          },
        },
      }
    }
    return { data: { status: 'success', data: {} } }
  })
})

afterEach(() => {
  window.history.pushState({}, '', '/')
  vi.clearAllMocks()
})

describe('TOTP claims', () => {
  it('the TOTP tab promises no backup codes and no password reset', async () => {
    window.history.pushState({}, '', '/profile?tab=totp')
    render(<Profile />)
    expect(await screen.findByText('Setup Instructions:')).toBeInTheDocument()
    const text = document.body.textContent ?? ''
    for (const claim of UNTRUE) expect(text).not.toMatch(claim)
    expect(text).toContain('There are no backup codes')
    expect(text).toContain('the codes OpenAlgo asks for when two-factor sign-in is on')
  })

  it('the setup page offers two-factor sign-in, not TOTP password resets', () => {
    render(<Setup />)
    const text = document.body.textContent ?? ''
    expect(text).not.toMatch(/password reset/i)
    expect(text).toContain('to set up two-factor sign-in')
  })
})
