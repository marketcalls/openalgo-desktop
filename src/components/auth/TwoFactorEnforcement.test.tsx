import { beforeEach, describe, expect, it, vi } from 'vitest'
import { render, screen } from '@/test/test-utils'

const client = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }))
vi.mock('@/api/client', () => ({ webClient: client }))

import TwoFactorEnforcement from './TwoFactorEnforcement'

describe('2FA Enforcement on the desktop', () => {
  beforeEach(() => {
    client.get.mockResolvedValue({
      data: {
        totp_enabled: true,
        totp_required_for_login: true,
        totp_required_for_mcp: true,
        totp_required_for_password_reset: true,
        last_totp_verified_at: null,
      },
    })
  })

  // docs-33: only dashboard sign-in reads its switch (auth_service reads
  // is_totp_required_for("login") alone). The Remote MCP and password reset
  // switches saved values nothing enforced, and the reset one spoke of an
  // email the desktop never sends.
  it('offers only the switch the desktop enforces', async () => {
    render(<TwoFactorEnforcement />)
    expect(await screen.findByText('Dashboard sign-in')).toBeInTheDocument()
    expect(screen.queryByText('Remote MCP authorization')).toBeNull()
    expect(screen.queryByText('Password reset')).toBeNull()
    expect(screen.queryByText(/email/i)).toBeNull()
    expect(screen.queryByRole('button', { name: /select all|clear all/i })).toBeNull()
    // The master switch and dashboard sign-in.
    expect(screen.getAllByRole('switch')).toHaveLength(2)
  })
})
