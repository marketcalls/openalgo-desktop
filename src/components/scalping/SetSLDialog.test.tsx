/**
 * UI-02: the stop-loss dialog closes only once the save or the removal is
 * acknowledged. The web closed it as soon as Save was pressed, so a failed
 * save left the trader believing the position was protected.
 */
import { describe, expect, it, vi } from 'vitest'
import type { SLState } from '@/hooks/useTrailingSL'
import { render, screen, userEvent, waitFor } from '@/test/test-utils'
import type { SelectedLeg } from '@/types/scalping'
import { SetSLDialog } from './SetSLDialog'

const leg: SelectedLeg = {
  symbol: 'NIFTY28OCT2625000CE',
  exchange: 'NFO',
  optionType: 'CE',
  strike: 25000,
  lotsize: 75,
  tickSize: 0.05,
}

const existing: SLState = {
  symbol: leg.symbol,
  exchange: 'NFO',
  product: 'NRML',
  side: 'BUY',
  entry: 100,
  quantity: 75,
  initialSl: 90,
  trailingEnabled: false,
  trailingStep: 0,
  highestPrice: 100,
  lowestPrice: 100,
  currentSl: 90,
  target: 0,
  active: true,
}

function dialog(props: {
  onSave: (sl: SLState) => Promise<boolean>
  onClear?: () => Promise<boolean>
  existing?: SLState
}) {
  const onOpenChange = vi.fn()
  render(
    <SetSLDialog
      open
      onOpenChange={onOpenChange}
      leg={leg}
      product="NRML"
      side="BUY"
      entryPrice={100}
      quantity={75}
      ltp={100}
      {...props}
    />
  )
  return onOpenChange
}

describe('SetSLDialog waits for the server', () => {
  it('stays open when the save failed', async () => {
    const onSave = vi.fn().mockResolvedValue(false)
    const onOpenChange = dialog({ onSave })
    await userEvent.type(screen.getByLabelText('Stop-loss price'), '90')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(onSave).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled())
    expect(onOpenChange).not.toHaveBeenCalledWith(false)
  })

  it('closes once the save is acknowledged', async () => {
    const onSave = vi.fn().mockResolvedValue(true)
    const onOpenChange = dialog({ onSave })
    await userEvent.type(screen.getByLabelText('Stop-loss price'), '90')
    await userEvent.click(screen.getByRole('button', { name: 'Save' }))
    await waitFor(() => expect(onOpenChange).toHaveBeenCalledWith(false))
  })

  it('stays open when removing the stop failed', async () => {
    const onClear = vi.fn().mockResolvedValue(false)
    const onOpenChange = dialog({ onSave: vi.fn(), onClear, existing })
    await userEvent.click(screen.getByRole('button', { name: 'Clear SL' }))
    await waitFor(() => expect(onClear).toHaveBeenCalledTimes(1))
    await waitFor(() => expect(screen.getByRole('button', { name: 'Clear SL' })).toBeEnabled())
    expect(onOpenChange).not.toHaveBeenCalledWith(false)
  })
})
