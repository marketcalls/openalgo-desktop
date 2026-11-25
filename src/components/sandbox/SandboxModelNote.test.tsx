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
    expect(text).toMatch(/pays brokerage under your plan and the statutory charges/)
    expect(text).toMatch(/P&L is shown before charges, with the charges beside it/)
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
