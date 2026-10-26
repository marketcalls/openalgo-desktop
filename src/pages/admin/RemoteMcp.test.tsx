import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { showToast } from '@/utils/toast'

const api = vi.hoisted(() => ({
  getOAuthClients: vi.fn(),
  getMCPAudit: vi.fn(),
  getMCPSettings: vi.fn(),
  updateMCPSettings: vi.fn(),
  triggerMCPKillSwitch: vi.fn(),
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

  function renderPage() {
    return render(
      <MemoryRouter>
        <RemoteMcp />
      </MemoryRouter>
    )
  }

  // docs-9: the desktop applies a save at once; the web's "restart the
  // service" toast and banner said otherwise.
  it('applies a save at once and never asks for a restart', async () => {
    api.updateMCPSettings.mockResolvedValue({
      status: 'success',
      restart_required: false,
      settings_pending: {
        http_enabled: true,
        public_url: '',
        mcp_url: '',
        require_approval: true,
        write_scope_enabled: false,
      },
    })
    renderPage()
    expect(await screen.findByText('Currently disabled')).toBeInTheDocument()
    expect(screen.getByText('Remote MCP is currently disabled')).toBeInTheDocument()
    fireEvent.click(screen.getAllByRole('switch')[0])
    fireEvent.click(screen.getByRole('button', { name: /save changes/i }))
    await waitFor(() =>
      expect(showToast.success).toHaveBeenCalledWith('Saved. The new settings apply now.', 'admin')
    )
    expect(await screen.findByText('Currently enabled')).toBeInTheDocument()
    expect(screen.queryByText('Remote MCP is currently disabled')).toBeNull()
    expect(document.body.textContent).not.toMatch(/restart/i)
    expect(localStorage.getItem('mcp_restart_pending_settings')).toBeNull()
  })

  it('clears a restart note an earlier version left behind', async () => {
    localStorage.setItem(
      'mcp_restart_pending_settings',
      JSON.stringify({
        http_enabled: true,
        public_url: '',
        mcp_url: '',
        require_approval: true,
        write_scope_enabled: true,
      })
    )
    renderPage()
    expect(await screen.findByText('Remote MCP settings')).toBeInTheDocument()
    expect(document.body.textContent).not.toMatch(/restart/i)
    expect(localStorage.getItem('mcp_restart_pending_settings')).toBeNull()
  })

  // docs-9: no OAuth sign-in, refresh tokens, JWT claims or log file on the
  // desktop; the kill switch revokes access tokens and turns order tools off.
  it('describes the kill switch, the address and the audit as the desktop has them', async () => {
    renderPage()
    expect(await screen.findByText(/MCP audit in OpenAlgo's logs database/)).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: /kill switch/i }))
    const dialog = (await screen.findByText('Trigger the Remote MCP kill switch?')).closest(
      '[role="alertdialog"], [role="dialog"]'
    ) as HTMLElement
    expect(dialog).toHaveTextContent('revokes every MCP access token at once')
    expect(dialog).toHaveTextContent('turns off order placement for AI clients')
    const text = document.body.textContent ?? ''
    for (const untrue of ['mcp.jsonl', '/oauth/', 'JWT', 'refresh token', 'OAuth dance']) {
      expect(text, untrue).not.toContain(untrue)
    }
  })
})
