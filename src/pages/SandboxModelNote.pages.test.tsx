import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import Analyzer from './Analyzer'
import Sandbox from './Sandbox'

// The Analyzer (Sandbox Request Monitor) and Sandbox Configuration pages
// state the sandbox's limits where traders review sandbox results.
describe('Sandbox model note on the sandbox pages', () => {
  beforeEach(() => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(JSON.stringify({ status: 'error' }), { status: 200 }))
    )
  })
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('is on the Analyzer page', async () => {
    render(
      <MemoryRouter>
        <Analyzer />
      </MemoryRouter>
    )
    expect(await screen.findByText('Sandbox model')).toBeInTheDocument()
    expect(screen.getByText(/in full or not at all/)).toBeInTheDocument()
  })

  it('is on the Sandbox page', async () => {
    render(
      <MemoryRouter>
        <Sandbox />
      </MemoryRouter>
    )
    expect(await screen.findByText('Sandbox model')).toBeInTheDocument()
    expect(screen.getByText(/P&L is shown before charges/)).toBeInTheDocument()
  })
})
