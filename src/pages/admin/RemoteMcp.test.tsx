import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const api = vi.hoisted(() => ({
  getOAuthClients: vi.fn(),
  getMCPAudit: vi.fn(),
  getMCPSettings: vi.fn(),
  updateMCPSettings: vi.fn(),
}))

vi.mock('@/api/admin', () => ({ adminApi: api }))
vi.mock('@/utils/toast', () => ({
  showToast: { error: vi.fn(), success: vi.fn(), info: vi.fn(), warning: vi.fn() },
}))

import RemoteMcp from './RemoteMcp'

describe('Remote MCP page', () => {
  beforeEach(() => {
    localStorage.clear()
    api.getOAuthClients.mockResolvedValue({
      mcp_enabled: true,
      clients: [],
      summary: { pending: 0, approved: 0, revoked: 0 },
    })
    api.getMCPAudit.mockResolvedValue({ data: [], total_in_window: 0 })
    api.getMCPSettings.mockResolvedValue({
      status: 'success',
      settings: {
        http_enabled: false,
        public_url: '',
        mcp_url: '',
        require_approval: true,
        write_scope_enabled: false,
      },
    })
  })

  // Security review S-15: the approval setting is stored but nothing on the
  // desktop reads it yet, so the page says so instead of implying it works.
  it('says the client approval setting has no effect yet', async () => {
    render(
      <MemoryRouter>
        <RemoteMcp />
      </MemoryRouter>
    )
    expect(await screen.findByText(/so this setting has no effect/i)).toBeInTheDocument()
  })
})
