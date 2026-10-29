import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { checkA11y } from '@/test/a11y-utils'
import { DeleteWorkflowDialog, RegenerateWebhookDialog } from './FlowConfirmDialogs'

describe('DeleteWorkflowDialog', () => {
  it('names the workflow and what goes with it, and deletes only on confirm', async () => {
    const onConfirm = vi.fn()
    const onOpenChange = vi.fn()
    render(
      <DeleteWorkflowDialog
        name="Opening range breakout"
        isActive={false}
        open
        onOpenChange={onOpenChange}
        onConfirm={onConfirm}
      />
    )
    expect(screen.getByText('Delete Opening range breakout?')).toBeInTheDocument()
    expect(screen.getByText(/schedule, its webhook URL and its run history/)).toBeInTheDocument()
    expect(screen.queryByText(/It is active now/)).not.toBeInTheDocument()
    await checkA11y(document.body)

    const user = userEvent.setup()
    await user.click(screen.getByRole('button', { name: 'Cancel' }))
    expect(onConfirm).not.toHaveBeenCalled()
    expect(onOpenChange).toHaveBeenCalledWith(false)
    await user.click(screen.getByRole('button', { name: 'Delete workflow' }))
    expect(onConfirm).toHaveBeenCalledTimes(1)
  })

  it('warns that an active workflow stops', () => {
    render(
      <DeleteWorkflowDialog name="ORB" isActive open onOpenChange={vi.fn()} onConfirm={vi.fn()} />
    )
    expect(
      screen.getByText(
        'It is active now: it stops, and alerts sent to its webhook URL are refused.'
      )
    ).toBeInTheDocument()
  })
})

describe('RegenerateWebhookDialog', () => {
  it('says the old URL stops working, and regenerates only on confirm', async () => {
    const onConfirm = vi.fn()
    render(<RegenerateWebhookDialog open onOpenChange={vi.fn()} onConfirm={onConfirm} />)
    expect(screen.getByText(/stops reaching this workflow/)).toBeInTheDocument()
    await checkA11y(document.body)
    await userEvent.setup().click(screen.getByRole('button', { name: 'Regenerate' }))
    expect(onConfirm).toHaveBeenCalledTimes(1)
  })
})
