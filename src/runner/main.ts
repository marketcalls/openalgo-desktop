/**
 * Desktop: the entry of the OpenScript runner page (`openscript-runner.html`),
 * which the app opens in a hidden window for each live run.
 */

import { Driver, RunGone } from './driver'
import { httpTransport, readFragment } from './transport'

async function main(): Promise<void> {
  const found = readFragment(window.location.hash)
  if (found === null) return
  // Keep the hidden page from being suspended where the platform would
  // otherwise do so: a pending Web Lock is held for the life of the run.
  void navigator.locks
    ?.request(`openscript-runner-${found.run}`, () => new Promise<void>(() => undefined))
    .catch(() => undefined)

  const transport = httpTransport(found.run, found.token)
  try {
    const spec = await transport.spec()
    document.title = `OpenScript ${spec.run.file}`
    const { loadText } = await import('openalgo-script')
    const driver = new Driver(transport, loadText, spec)
    if (!driver.load()) return
    let history = await transport.bars().catch((e: unknown) => {
      if (e instanceof RunGone) throw e
      driver.say(`History could not be read: ${e instanceof Error ? e.message : String(e)}`)
      return null
    })
    while (history === null) {
      await new Promise((r) => setTimeout(r, 15_000))
      history = await transport.bars().catch((e: unknown) => {
        if (e instanceof RunGone) throw e
        return null
      })
    }
    await driver.begin(history)
    await driver.flush()
    await driver.run()
  } catch (e) {
    if (e instanceof RunGone) return
    const message = `The strategy page stopped: ${e instanceof Error ? e.message : String(e)}`
    await transport.ended(message).catch(() => undefined)
  }
}

void main()
