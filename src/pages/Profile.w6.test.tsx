/**
 * The Flow toast switch on the Profile page's Alerts tab, rendered: it sits in
 * the Data & Automation card after Historify (the web's Python Strategy row is
 * not in the desktop), with its label and description, and it turns the
 * `flow` toast category off and on.
 *
 * Pins: F-104 F-407.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useAlertStore } from '@/stores/alertStore'
import { render, screen, userEvent, within } from '@/test/test-utils'

const client = vi.hoisted(() => ({
  get: vi.fn(),
  post: vi.fn(),
}))

vi.mock('@/api/client', () => ({ webClient: client }))

import Profile from './Profile'

/** A card on the page, found by its title. */
function cardTitled(title: string) {
  const heading = screen.getByText(title, { selector: '[data-slot="card-title"]' })
  return heading.closest('[data-slot="card"]') as HTMLElement
}

/** The category rows of a card: each one's label and description. */
function rowsOf(card: HTMLElement) {
  return within(card)
    .getAllByRole('switch')
    .map((toggle) => {
      const row = toggle.parentElement as HTMLElement
      return [row.querySelector('label')?.textContent, row.querySelector('p')?.textContent]
    })
}

/** The Broker tab's saved addresses, as Profile.test.tsx serves them. */
const credentials = {
  broker_api_key: 'kiteap********',
  broker_api_secret: 'kite********',
  redirect_url: 'http://127.0.0.1:5000/zerodha/callback',
  current_broker: 'zerodha',
  valid_brokers: ['zerodha'],
  client_id_brokers: [],
  client_id: null,
  ngrok_allow: false,
  host_server: 'http://127.0.0.1:5000',
  websocket_url: 'ws://127.0.0.1:8765',
}

beforeEach(() => {
  window.history.pushState({}, '', '/profile?tab=alerts')
  client.get.mockImplementation(async (url: string) => {
    if (url === '/api/broker/credentials') return { data: { status: 'success', data: credentials } }
    if (url === '/auth/profile-data') {
      return { data: { status: 'success', data: { username: 'trader', smtp_settings: null } } }
    }
    return { data: { status: 'success', data: {} } }
  })
  useAlertStore.getState().resetToDefaults()
})

afterEach(() => {
  window.history.pushState({}, '', '/')
  useAlertStore.getState().resetToDefaults()
  vi.clearAllMocks()
})

describe('F-104 F-407 the Flow toast category', () => {
  it('is the second row of the Data & Automation card, after Historify, in these words', async () => {
    render(<Profile />)
    await screen.findByText('Data & Automation', { selector: '[data-slot="card-title"]' })
    const card = cardTitled('Data & Automation')
    expect(
      within(card).getByText('Notifications from historical data and workflow automation')
    ).toBeInTheDocument()
    expect(rowsOf(card)).toEqual([
      ['Historify', 'Historical data jobs, file uploads, downloads, and schedules'],
      ['Flow Workflows', 'Visual workflow creation, execution, and management'],
    ])
    // Only there: no other card carries a Flow row.
    expect(screen.getAllByText('Flow Workflows')).toHaveLength(1)
  })

  it('its switch turns the flow toasts off and on', async () => {
    render(<Profile />)
    await screen.findByText('Data & Automation', { selector: '[data-slot="card-title"]' })
    const toggle = within(cardTitled('Data & Automation')).getByRole('switch', {
      name: 'Flow Workflows',
    })
    expect(toggle).toHaveAttribute('aria-checked', 'true')
    const user = userEvent.setup()
    await user.click(toggle)
    expect(useAlertStore.getState().categories.flow).toBe(false)
    expect(useAlertStore.getState().shouldShowToast('flow')).toBe(false)
    expect(useAlertStore.getState().categories.historify).toBe(true)
    expect(toggle).toHaveAttribute('aria-checked', 'false')
    await user.click(toggle)
    expect(useAlertStore.getState().categories.flow).toBe(true)
  })
})
