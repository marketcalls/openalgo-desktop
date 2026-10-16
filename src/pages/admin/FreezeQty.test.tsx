/**
 * The Freeze Quantities page says what its list is on the desktop: the
 * trader's own entries over NSE's built-in limits, so an empty list is not
 * read as "no limits", and says where an entry takes effect.
 */
import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@/test/test-utils'
import FreezeQty from './FreezeQty'

vi.mock('@/api/admin', () => ({
  adminApi: { getFreezeList: vi.fn().mockResolvedValue([]) },
}))

describe('FreezeQty on the desktop', () => {
  it("says NSE limits are built in and the list holds the trader's own entries", async () => {
    render(<FreezeQty />)
    expect(
      await screen.findByText(/NSE's freeze quantities for NFO are built into OpenAlgo Desktop/)
    ).toBeInTheDocument()
    expect(screen.getByText(/scalping order checks and exits/)).toBeInTheDocument()
  })
})
