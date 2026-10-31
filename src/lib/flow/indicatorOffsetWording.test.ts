/**
 * IND-083: the Indicator node's bars-back wording. The web's panel said
 * `0 = latest closed bar.`, but the latest bar the run reads is usually still
 * forming; the panel (ConfigPanel.desktop.test.tsx pins it), the import-format
 * doc and the user guide all say the same corrected thing, pinned here word for
 * word so a later edit to one cannot leave the others behind.
 */

import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

const read = (path: string) => readFileSync(join(process.cwd(), path), 'utf8').replace(/\s+/g, ' ')

describe('bars back on the Indicator node, as the docs say it (IND-083)', () => {
  it('the panel says 0 may still be forming and 1 is the last closed bar', () => {
    expect(read('src/components/flow/panels/ConfigPanel.tsx')).toContain(
      '0 = latest bar, which may still be forming; 1 = the last closed bar.'
    )
  })

  it('the import-format doc gives offsetBars the same meaning', () => {
    expect(read('docs/prompt/flow-import-format.md')).toContain(
      '| `offsetBars` | int | `0` | Which bar `at_offset` reads: 0 = the latest bar, which may still be forming; 1 = the last closed bar. |'
    )
  })

  it('the user guide tells the trader to read the previous bar for a closed one', () => {
    expect(read('docs/userguide/21-flow-visual-builder/README.md')).toContain(
      "The Indicator node's latest bar is usually the one still forming; use `{{name.previous.*}}` (or **Offset Bars** 1) for the last closed bar."
    )
  })

  it('no longer says the web\'s "latest closed bar"', () => {
    for (const path of [
      'src/components/flow/panels/ConfigPanel.tsx',
      'docs/prompt/flow-import-format.md',
      'docs/userguide/21-flow-visual-builder/README.md',
    ]) {
      expect(read(path), path).not.toContain('0 = latest closed bar')
    }
  })
})
