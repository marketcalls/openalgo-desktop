/**
 * Every row and tip of /flow/shortcuts, verbatim (parity F-180, F-181,
 * F-182; the right-click tip is dropped on the desktop, F-182).
 */

import { render, screen, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { describe, expect, it } from 'vitest'
import FlowKeyboardShortcuts from './FlowKeyboardShortcuts'

const SECTIONS: Array<[string, Array<[string[], string]>]> = [
  [
    'General',
    [
      [['Ctrl', 'S'], 'Save workflow'],
      [['Escape'], 'Deselect node/edge'],
      [['?'], 'Open keyboard shortcuts'],
    ],
  ],
  [
    'Canvas Navigation',
    [
      [['Scroll'], 'Zoom in/out'],
      [['Drag'], 'Pan canvas'],
      [['Ctrl', 'Scroll'], 'Zoom in/out (precise)'],
    ],
  ],
  [
    'Node Operations',
    [
      [['Click'], 'Select node'],
      [['Delete'], 'Delete selected node'],
      [['Backspace'], 'Delete selected node'],
      [['Drag from palette'], 'Add new node'],
    ],
  ],
  [
    'Edge Operations',
    [
      [['Click edge'], 'Select edge'],
      [['Delete'], 'Delete selected edge'],
      [['Backspace'], 'Delete selected edge'],
      [['Drag from handle'], 'Create connection'],
    ],
  ],
  [
    'Selection',
    [
      [['Click node'], 'Select single node'],
      [['Click edge'], 'Select single edge'],
      [['Click canvas'], 'Clear selection'],
    ],
  ],
  [
    'Workflow Controls',
    [
      [['Ctrl', 'S'], 'Save changes'],
      [['Run Now'], 'Execute workflow immediately'],
    ],
  ],
]

function renderPage() {
  render(
    <MemoryRouter>
      <FlowKeyboardShortcuts />
    </MemoryRouter>
  )
}

describe('/flow/shortcuts', () => {
  it('heads the page with its subtitle (F-180)', () => {
    renderPage()
    expect(screen.getByRole('heading', { level: 1, name: 'Keyboard Shortcuts' })).toBeVisible()
    expect(
      screen.getByText('Quick reference for all available keyboard shortcuts')
    ).toBeInTheDocument()
  })

  it.each(SECTIONS)('the %s section lists its keys and actions in order (F-181)', (title, rows) => {
    renderPage()
    const section = screen.getByRole('heading', { level: 2, name: title })
      .parentElement as HTMLElement
    const drawn = [...section.querySelectorAll(':scope > div > div')].map((row) => [
      [...row.querySelectorAll('kbd')].map((k) => k.textContent),
      row.querySelector('span')?.textContent,
    ])
    expect(drawn).toEqual(rows)
    // Keys of a combination are joined with a plus.
    for (const [keys] of rows.filter(([k]) => k.length > 1)) {
      expect(within(section).getAllByText('+').length).toBeGreaterThan(0)
      expect(keys).toHaveLength(2)
    }
  })

  it('ends with the tips, without the context menu the editor does not have (F-182)', () => {
    renderPage()
    const tips = screen.getByRole('heading', { level: 3, name: 'Tips' })
      .parentElement as HTMLElement
    expect([...tips.querySelectorAll('li')].map((li) => li.textContent)).toEqual([
      'On Mac, use Cmd instead of Ctrl for keyboard shortcuts',
      'Click the + button on edges to insert a new node between existing nodes',
      'Drag nodes to reposition them on the canvas',
      'Use the minimap in the bottom-right corner for quick navigation',
    ])
  })
})
