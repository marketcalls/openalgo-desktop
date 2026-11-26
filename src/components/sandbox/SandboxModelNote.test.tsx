import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { SANDBOX_MODEL_POINTS, SandboxModelNote } from './SandboxModelNote'

describe('SandboxModelNote', () => {
  it('states every limit of the simulation', () => {
    render(<SandboxModelNote />)
    expect(screen.getByText('Sandbox model')).toBeInTheDocument()
    const text = screen.getByRole('alert').textContent ?? ''
    for (const point of SANDBOX_MODEL_POINTS) {
      expect(text).toContain(point)
    }
    expect(text).toMatch(/in full or not at all/)
    expect(text).toMatch(
      /On the Indian exchanges each fill pays brokerage under your plan and the statutory charges/
    )
    expect(text).toMatch(/come out of the available balance unless they are switched off/)
    expect(text).toMatch(/P&L is shown before charges, with the charges beside it/)
  })

  // A crypto venue (Delta Exchange) charges the venue fee alone, and nothing
  // until the trader enters one (web marketcalls/openalgo#2496, 4bce67c35).
  // The note shows whatever the broker, so it must not tell a crypto trader
  // that a fill paid brokerage and statutory charges.
  it('says a crypto venue pays only the venue fee', () => {
    render(<SandboxModelNote />)
    const text = screen.getByRole('alert').textContent ?? ''
    expect(text).toMatch(/on a crypto venue it pays only the venue fee you set/)
    expect(text).toMatch(/nothing until you enter one/)
    expect(text).not.toMatch(/Each fill pays brokerage under your plan and the statutory charges/)
    expect(text).toMatch(/contract value/)
    expect(text).toMatch(/rupees.*US dollars/)
    expect(text).toMatch(/next calendar day/)
    expect(text).toMatch(/prices from your broker/)
  })

  it('uses trader vocabulary', () => {
    render(<SandboxModelNote />)
    const text = (screen.getByRole('alert').textContent ?? '').toLowerCase()
    expect(text).not.toMatch(/paper trading|virtual trading/)
  })
})
