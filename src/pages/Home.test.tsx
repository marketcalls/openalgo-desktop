/**
 * The Home page's Flow desk card, rendered: its title and its whole sentence,
 * among the platform's desks (config/scope.test.ts ties the node count in the
 * sentence to the palette).
 *
 * Pins: D-55.
 */

import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { describe, expect, it, vi } from 'vitest'

vi.mock('@/components/layout/Footer', () => ({ Footer: () => null }))

import Home from './Home'

describe('D-55 the Flow card on the Home page', () => {
  it('is titled No-code strategy builder and says what Flow does in one sentence', () => {
    render(
      <MemoryRouter>
        <Home />
      </MemoryRouter>
    )
    const title = screen.getByRole('heading', { level: 3, name: 'No-code strategy builder' })
    expect(title.nextElementSibling?.tagName).toBe('P')
    expect(title.nextElementSibling?.textContent).toBe(
      'Flow is a drag-and-drop canvas: wire 61 node types from triggers to indicators, conditions and orders, then activate it to run on its own.'
    )
    // Third of the six desks, as on the web: after charting, before options.
    const desks = screen.getAllByRole('heading', { level: 3 }).map((h) => h.textContent)
    expect(desks.slice(0, 6)).toEqual([
      'Self-hosted algo trading',
      'Charting platform',
      'No-code strategy builder',
      'Options trading platform',
      'Scalping platform',
      'Sandbox testing',
    ])
  })
})
