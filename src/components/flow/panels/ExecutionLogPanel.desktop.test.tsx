import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { ExecutionLogPanel, type LogEntry } from './ExecutionLogPanel'

describe('ExecutionLogPanel on the desktop', () => {
  it("shows a Log node's warn level as a warning, not as a success (F-643)", () => {
    // The Log node stores `warn`, and the run passes it through as the level.
    const warn = { time: '2026-10-30T09:20:00Z', message: 'Spread is wide', level: 'warn' }
    render(
      <ExecutionLogPanel logs={[warn as unknown as LogEntry]} status="success" onClose={() => {}} />
    )
    const line = screen.getByText('Spread is wide')
    expect(line.className).toContain('text-amber-500')
    expect(line.closest('.rounded-lg')?.className).toContain('border-amber-500/30')
  })
})
