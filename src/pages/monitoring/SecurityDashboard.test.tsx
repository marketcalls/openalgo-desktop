import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@/test/test-utils'
import SecurityDashboard from './SecurityDashboard'

const http = vi.hoisted(() => ({
  get: vi.fn(async (url: string) => {
    if (url === '/security/api/data') {
      return {
        data: {
          banned_ips: [],
          suspicious_ips: [],
          api_abuse_ips: [],
          security_settings: {
            auto_ban_enabled: true,
            '404_threshold': 100,
            '404_ban_duration': 0,
            api_threshold: 100,
            api_ban_duration: 0,
            repeat_offender_limit: 2,
          },
        },
      }
    }
    if (url === '/security/stats') {
      return {
        data: {
          total_bans: 0,
          permanent_bans: 0,
          temporary_bans: 0,
          suspicious_ips: 0,
          near_threshold: 0,
        },
      }
    }
    return { data: { status: 'success', attempts: [], sessions: [], current_session_id: null } }
  }),
}))

vi.mock('@/api/client', () => ({
  webClient: { get: http.get, post: vi.fn() },
}))

describe('Security Dashboard', () => {
  // docs-47: the Repeat Limit makes only the bans entered on this page
  // permanent; an automatic ban always expires (G3). The help said it
  // applied to every ban.
  it('says the Repeat Limit applies to the bans entered on the page', async () => {
    render(<SecurityDashboard />)
    expect(
      await screen.findByText(
        /for the bans you enter on this page\. Automatic bans always expire\./
      )
    ).toBeInTheDocument()
  })
})
