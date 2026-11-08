import { describe, expect, it } from 'vitest'
import { DESKTOP_LATEST_RELEASE_URL } from '@/components/desktop/DownloadThisApp'
import { render, screen, within } from '@/test/test-utils'
import Download from './Download'

function cardOf(name: string): HTMLElement {
  const heading = screen.getByText(name, { selector: '[data-slot="card-title"]' })
  const card = heading.closest<HTMLElement>('[data-slot="card"]')
  if (!card) throw new Error(`no card for ${name}`)
  return card
}

describe('Download page', () => {
  // Desktop: the web's OpenAlgo Desktop card offers installers of the version
  // written into the page. Inside the app it would offer the app the trader
  // is running, out of date after the next update.
  it('offers no OpenAlgo Desktop installer and says updates come in the app', () => {
    render(<Download />)
    const hrefs = screen.getAllByRole('link').map((link) => link.getAttribute('href') ?? '')
    expect(hrefs.filter((href) => href.includes('openalgo-desktop/releases/download'))).toEqual([])

    const thisApp = cardOf('OpenAlgo Desktop')
    expect(thisApp).toHaveTextContent('This is the app you are using.')
    expect(thisApp).toHaveTextContent('New versions come as an in-app update')
    expect(within(thisApp).getByRole('link', { name: /Release page/ })).toHaveAttribute(
      'href',
      DESKTOP_LATEST_RELEASE_URL
    )
    expect(within(cardOf('FastScalper Desktop')).getAllByRole('link').length).toBeGreaterThan(1)
  })
})
