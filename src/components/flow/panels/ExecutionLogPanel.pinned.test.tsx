/**
 * The execution log panel, pinned where the audit found the web's behaviour
 * untested: the status icon and pill for every run state, the close button,
 * the empty state, the entry tints and icons, the time format and the footer.
 *
 * Pins: F-641 F-642.
 */

import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { ExecutionLogPanel, type ExecutionStatus, type LogEntry } from './ExecutionLogPanel'

function header(container: HTMLElement) {
  const title = within(container).getByText('Execution Log')
  return title.parentElement as HTMLElement
}

function classOf(element: Element | null | undefined) {
  return element?.getAttribute('class') ?? ''
}

describe('F-641 the panel header', () => {
  const states: Array<[ExecutionStatus, string, string[], string[]]> = [
    [
      'running',
      'Executing...',
      ['lucide-clock', 'animate-pulse', 'text-amber-500'],
      ['bg-amber-500/10', 'text-amber-500'],
    ],
    [
      'started',
      'Running in background',
      ['lucide-clock', 'text-amber-500'],
      ['bg-amber-500/10', 'text-amber-500'],
    ],
    [
      'success',
      'Completed',
      ['lucide-circle-check', 'text-green-500'],
      ['bg-green-500/10', 'text-green-500'],
    ],
    ['error', 'Failed', ['lucide-circle-x', 'text-red-500'], ['bg-red-500/10', 'text-red-500']],
    [
      'idle',
      'Ready',
      ['lucide-terminal', 'text-muted-foreground'],
      ['bg-muted', 'text-muted-foreground'],
    ],
  ]

  it.each(states)('%s shows %s with its icon and colour', (status, text, icon, pill) => {
    const { container } = render(<ExecutionLogPanel logs={[]} status={status} onClose={() => {}} />)
    const statusIcon = header(container).querySelector('svg')
    for (const cls of icon) expect(classOf(statusIcon)).toContain(cls)
    const label = screen.getByText(text)
    for (const cls of pill) expect(label).toHaveClass(cls)
  })

  it('a started run does not pulse, a running one does', () => {
    const { container, rerender } = render(
      <ExecutionLogPanel logs={[]} status="started" onClose={() => {}} />
    )
    expect(classOf(header(container).querySelector('svg'))).not.toContain('animate-pulse')
    rerender(<ExecutionLogPanel logs={[]} status="running" onClose={() => {}} />)
    expect(classOf(header(container).querySelector('svg'))).toContain('animate-pulse')
  })

  it('the close button closes the panel', async () => {
    const onClose = vi.fn()
    render(<ExecutionLogPanel logs={[]} status="idle" onClose={onClose} />)
    await userEvent.setup().click(screen.getByRole('button', { name: 'Close execution log' }))
    expect(onClose).toHaveBeenCalledTimes(1)
  })
})

describe('F-642 the log entries', () => {
  const at = '2026-10-30T09:20:05'

  it('says there are no logs yet and how to get some, with no footer', () => {
    render(<ExecutionLogPanel logs={[]} status="idle" onClose={() => {}} />)
    expect(screen.getByText('No logs yet')).toBeInTheDocument()
    expect(screen.getByText('Click "Run Now" to execute the workflow')).toBeInTheDocument()
    expect(screen.queryByText(/log entries/)).not.toBeInTheDocument()
  })

  it('tints each entry by level, with its icon, and colours the message', () => {
    const logs: LogEntry[] = [
      { time: at, message: 'Quote received', level: 'info' },
      { time: at, message: 'Spread is wide', level: 'warning' },
      { time: at, message: 'Order rejected: RMS', level: 'error' },
    ]
    render(<ExecutionLogPanel logs={logs} status="error" onClose={() => {}} />)

    const info = screen.getByText('Quote received')
    expect(info.closest('.rounded-lg')).toHaveClass('border-border', 'bg-background')
    expect(info.className).not.toMatch(/text-(red|amber)-500/)
    expect(classOf(info.closest('.rounded-lg')?.querySelector('svg'))).toContain(
      'lucide-circle-check'
    )

    const warning = screen.getByText('Spread is wide')
    expect(warning.closest('.rounded-lg')).toHaveClass('border-amber-500/30', 'bg-amber-500/5')
    expect(warning).toHaveClass('text-amber-500')
    expect(classOf(warning.closest('.rounded-lg')?.querySelector('svg'))).toContain(
      'lucide-circle-alert'
    )

    const error = screen.getByText('Order rejected: RMS')
    expect(error.closest('.rounded-lg')).toHaveClass('border-red-500/30', 'bg-red-500/5')
    expect(error).toHaveClass('text-red-500')
    expect(classOf(error.closest('.rounded-lg')?.querySelector('svg'))).toContain('lucide-circle-x')
  })

  it('shows the time as 24-hour hh:mm:ss', () => {
    render(
      <ExecutionLogPanel
        logs={[
          { time: '2026-10-30T09:20:05', message: 'Morning', level: 'info' },
          { time: '2026-10-30T15:04:09.123456', message: 'Afternoon', level: 'info' },
        ]}
        status="success"
        onClose={() => {}}
      />
    )
    expect(screen.getByText('09:20:05')).toBeInTheDocument()
    expect(screen.getByText('15:04:09')).toBeInTheDocument()
  })

  // The web's behaviour, carried as is: formatTime's catch, which would show
  // the raw text, never runs, because an unparseable time is an Invalid Date
  // and toLocaleTimeString formats that as the words "Invalid Date" rather
  // than throwing. A bare time of day is unparseable too.
  it.each(['', 'not a time', '09:20:05'])(
    'shows the web\'s "Invalid Date" for the unparseable time %j',
    (time) => {
      render(
        <ExecutionLogPanel
          logs={[{ time, message: 'Quote received', level: 'info' }]}
          status="success"
          onClose={() => {}}
        />
      )
      const shown = screen.getByText('Quote received').previousElementSibling
      expect(shown?.textContent).toBe('Invalid Date')
    }
  )

  it('counts the entries and the errors in the footer', () => {
    const logs: LogEntry[] = [
      { time: at, message: 'one', level: 'info' },
      { time: at, message: 'two', level: 'error' },
      { time: at, message: 'three', level: 'warning' },
      { time: at, message: 'four', level: 'error' },
    ]
    render(<ExecutionLogPanel logs={logs} status="error" onClose={() => {}} />)
    expect(screen.getByText('4 log entries')).toBeInTheDocument()
    expect(screen.getByText('2 errors')).toBeInTheDocument()
  })

  it('never shows the node an entry came from', () => {
    render(
      <ExecutionLogPanel
        logs={[{ time: at, message: 'Order placed', level: 'info', node: 'node_5' }]}
        status="success"
        onClose={() => {}}
      />
    )
    expect(screen.getByText('Order placed')).toBeInTheDocument()
    expect(screen.queryByText(/node_5/)).not.toBeInTheDocument()
  })
})
