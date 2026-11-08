import { ExternalLink } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Card, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'

/** The newest release's page, which lists every installer and its checksums. */
export const DESKTOP_LATEST_RELEASE_URL =
  'https://github.com/marketcalls/openalgo-desktop/releases/latest'

/**
 * Desktop-only: the Download page's OpenAlgo Desktop card. The web's first
 * card offers the OpenAlgo Desktop installers of one version written into
 * the page. Inside the app that would offer the trader the app they are
 * running, at a version that is out of date after the next update, so the
 * desktop shows this instead: new versions come as an in-app update, and
 * the release page has the installers for another computer.
 */
export function DownloadThisApp() {
  return (
    <Card id="openalgo-desktop" className="scroll-mt-24">
      <CardHeader>
        <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div className="space-y-1.5">
            <CardTitle className="text-2xl" role="heading" aria-level={2}>
              OpenAlgo Desktop
            </CardTitle>
            <CardDescription>
              This is the app you are using. New versions come as an in-app update, so there is
              nothing to download here. To install OpenAlgo Desktop on another computer, download it
              from the release page.
            </CardDescription>
          </div>
          <Button variant="outline" size="sm" asChild className="shrink-0">
            <a href={DESKTOP_LATEST_RELEASE_URL} target="_blank" rel="noopener noreferrer">
              <ExternalLink className="mr-2 h-4 w-4" />
              Release page
            </a>
          </Button>
        </div>
      </CardHeader>
    </Card>
  )
}
