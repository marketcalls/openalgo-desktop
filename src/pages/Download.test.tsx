import { axe, toHaveNoViolations } from 'jest-axe'
import { describe, expect, it } from 'vitest'
import { DESKTOP_LATEST_RELEASE_URL } from '@/components/desktop/DownloadThisApp'
import { render, screen, within } from '@/test/test-utils'
import Download from './Download'

expect.extend(toHaveNoViolations)

function cardOf(name: string): HTMLElement {
  const heading = screen.getByText(name, { selector: '[data-slot="card-title"]' })
  const card = heading.closest<HTMLElement>('[data-slot="card"]')
  if (!card) throw new Error(`no card for ${name}`)
  return card
}

describe('Download page', () => {
  // Web b592e914f: the companion apps, each with its explainer and download.
  it('offers FastScalper, the Chrome Plugin and the Excel Add-in', () => {
    render(<Download />)
    const downloads = screen.getByRole('navigation', { name: 'Downloads' })
    expect(
      within(downloads)
        .getAllByRole('link')
        .map((link) => link.textContent)
    ).toEqual(['FastScalper Desktop', 'Chrome Plugin', 'Excel Add-in'])

    expect(
      within(cardOf('Chrome Plugin')).getByRole('link', { name: /Download ZIP/ })
    ).toHaveAttribute(
      'href',
      'https://github.com/marketcalls/openalgo-chrome/releases/download/v1.0/openalgo-chrome.zip'
    )
    expect(
      within(cardOf('Excel Add-in')).getByRole('link', { name: /Download EXE/ })
    ).toHaveAttribute(
      'href',
      'https://github.com/marketcalls/OpenAlgo-Excel/releases/download/v1.0.7/OpenAlgoSetup.exe'
    )
    expect(within(cardOf('FastScalper Desktop')).getAllByRole('link').length).toBeGreaterThan(1)
  })

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
    expect(screen.getByText(/On this computer, your OpenAlgo is this app\./)).toBeVisible()
  })

  it('has no accessibility violations', async () => {
    const view = render(<Download />)
    expect(await axe(view.container)).toHaveNoViolations()
  })
})
