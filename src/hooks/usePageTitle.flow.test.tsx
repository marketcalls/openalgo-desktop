/**
 * The window title on the Flow pages (parity D-51, F-103, F-404), as the
 * web's usePageTitle names them.
 */

import { render } from '@testing-library/react'
import { MemoryRouter } from 'react-router'
import { describe, expect, it } from 'vitest'
import { usePageTitle } from './usePageTitle'

function TitleProbe() {
  usePageTitle()
  return null
}

function titleAt(path: string): string {
  render(
    <MemoryRouter initialEntries={[path]}>
      <TitleProbe />
    </MemoryRouter>
  )
  return document.title
}

describe('Flow page titles', () => {
  it.each([
    ['/flow', 'Flow | OpenAlgo'],
    ['/flow/shortcuts', 'Flow Shortcuts | OpenAlgo'],
    ['/flow/editor/7', 'Flow Editor | OpenAlgo'],
    ['/flow/editor/opening-range', 'Flow Editor | OpenAlgo'],
  ])('%s reads "%s"', (path, title) => {
    expect(titleAt(path)).toBe(title)
  })

  it('names no title for a deeper editor path', () => {
    expect(titleAt('/flow/editor/7/extra')).toBe('OpenAlgo')
  })
})
