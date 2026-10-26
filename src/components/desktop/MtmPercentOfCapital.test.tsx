import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { MtmPercentOfCapital } from './MtmPercentOfCapital'

// docs-49: the web reads the MTM against a fixed 1,00,000.
describe('MtmPercentOfCapital', () => {
  it('reads the MTM against the sandbox capital in analyzer mode', () => {
    const { container } = render(
      <MtmPercentOfCapital mtm={25000} capital={10_000_000} basis="sandbox" />
    )
    expect(container.textContent).toBe('+0.25% of sandbox capital')
    expect(container.firstElementChild).toHaveClass('text-green-500')
  })

  it('reads a loss against the account margin live', () => {
    const { container } = render(
      <MtmPercentOfCapital mtm={-1275} capital={127500} basis="account" />
    )
    expect(container.textContent).toBe('-1.00% of account margin')
    expect(container.firstElementChild).toHaveClass('text-red-500')
  })

  it('shows no percentage when the capital could not be read, and says why', () => {
    render(<MtmPercentOfCapital mtm={5000} capital={null} basis={null} />)
    expect(screen.getByText(/funds could not be read/)).toBeInTheDocument()
    expect(screen.queryByText(/%\s*of/)).toBeNull()
  })

  it('shows a placeholder before the first load', () => {
    const { container } = render(
      <MtmPercentOfCapital mtm={0} capital={undefined} basis={undefined} />
    )
    expect(container.textContent).toBe('--')
  })
})
