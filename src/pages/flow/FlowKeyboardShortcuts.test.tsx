import { render, screen, within } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { describe, expect, it } from 'vitest'
import { checkA11y } from '@/test/a11y-utils'
import FlowKeyboardShortcuts from './FlowKeyboardShortcuts'

function renderPage() {
  return render(
    <MemoryRouter initialEntries={['/flow/shortcuts']}>
      <FlowKeyboardShortcuts />
    </MemoryRouter>
  )
}

describe('/flow/shortcuts', () => {
  it('lists the editor shortcuts by section, as on the web', () => {
    renderPage()
    expect(screen.getByRole('heading', { level: 1, name: 'Keyboard Shortcuts' })).toBeVisible()
    expect(screen.getAllByRole('heading', { level: 2 }).map((h) => h.textContent)).toEqual([
      'General',
      'Canvas Navigation',
      'Node Operations',
      'Edge Operations',
      'Selection',
      'Workflow Controls',
    ])
    const save = screen.getByText('Save workflow').parentElement as HTMLElement
    expect(within(save).getByText('Ctrl')).toBeInTheDocument()
    expect(within(save).getByText('S')).toBeInTheDocument()
  })

  it('leads back to the workflow list', () => {
    renderPage()
    expect(screen.getByRole('link', { name: 'Back to flows list' })).toHaveAttribute(
      'href',
      '/flow'
    )
  })

  it('passes the accessibility checks', async () => {
    const { container } = renderPage()
    await checkA11y(container)
  })
})
