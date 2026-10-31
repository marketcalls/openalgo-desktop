/**
 * The approval card waits for the turn to stop running.
 *
 * The card arrives on the stream's confirm frame, a moment before the turn
 * stops running, and `confirm` ignores a decision while a turn runs. A click
 * in that moment was dropped with nothing to show for it (seen as the Flow
 * e2e journey 6 failing in CI 41178925634: Approve clicked, no request sent).
 */

import { screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { createAgentMessage } from '@/lib/agent/useAgentStream'
import { renderWithQuery } from '@/test/queryRender'

const client = vi.hoisted(() => ({ get: vi.fn(), post: vi.fn() }))
vi.mock('@/api/client', () => ({ webClient: client }))

import { Message } from './Message'

const paused = createAgentMessage('assistant', '', {
  pending: {
    runId: 'run-1',
    sessionId: 'session-1',
    requirements: [
      {
        id: 'req-1',
        tool_call_id: 'call-1',
        tool_name: 'cancel_all_orders',
        args: {},
        missing: [],
      },
    ],
  },
})

describe('the approval card', () => {
  it('cannot be decided while the turn is still running', () => {
    const onConfirm = vi.fn()
    renderWithQuery(<Message message={paused} onConfirm={onConfirm} busy />)

    expect(screen.getByText('This turn is waiting for your approval.')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Approve' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Reject' })).toBeDisabled()
  })

  it('is decided once the turn has stopped', async () => {
    const onConfirm = vi.fn()
    const { rerender } = renderWithQuery(<Message message={paused} onConfirm={onConfirm} busy />)
    rerender(<Message message={paused} onConfirm={onConfirm} busy={false} />)

    const approve = screen.getByRole('button', { name: 'Approve' })
    expect(approve).toBeEnabled()
    expect(screen.getByRole('button', { name: 'Reject' })).toBeEnabled()
    await userEvent.click(approve)
    expect(onConfirm).toHaveBeenCalledWith({ 'req-1': true })
  })
})
