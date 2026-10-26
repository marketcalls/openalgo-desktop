import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import {
  BROKER_COUNT,
  brokerNamesSentence,
  SECURITIES_BROKER_COUNT,
  SUPPORTED_BROKERS,
} from './brokerCatalog'

const ROOT = process.cwd()

/** The ids in the server's catalogue (`ALL_BROKERS`), read from its source. */
function catalogueIds(): string[] {
  const src = readFileSync(join(ROOT, 'src-tauri/src/brokers/catalog.rs'), 'utf8')
  const list = /pub const ALL_BROKERS: &\[&str\] = &\[([\s\S]*?)\];/.exec(src)?.[1] ?? ''
  return [...list.matchAll(/"([a-z_]+)"/g)].map((m) => m[1])
}

function markdownUnder(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return markdownUnder(path)
    return name.endsWith('.md') ? [path] : []
  })
}

describe('the broker catalogue the FAQ shows', () => {
  // docs-39: the FAQ said 36 brokers and left Rupeezy out.
  it('is the server catalogue, broker for broker', () => {
    const ids = catalogueIds()
    expect(ids.length).toBeGreaterThan(30)
    expect(SUPPORTED_BROKERS.map((b) => b.id).sort()).toEqual([...ids].sort())
    expect(BROKER_COUNT).toBe(ids.length)
    expect(SECURITIES_BROKER_COUNT).toBe(ids.length - 1)
  })

  it('names Rupeezy and reads as one list', () => {
    const sentence = brokerNamesSentence()
    expect(sentence).toContain('RMoney, Rupeezy, Samco')
    expect(sentence.startsWith('5 Paisa, 5 Paisa (XTS), ')).toBe(true)
    expect(sentence.endsWith(', Zebu and Zerodha')).toBe(true)
  })

  // The guides and the README say how many brokers there are; the number is
  // the catalogue's. The 2026-10-03 audit is a record of its day and keeps
  // the counts it found.
  it('is the count every guide and the README give', () => {
    const files = [
      'README.md',
      'CLAUDE.md',
      ...markdownUnder(join(ROOT, 'docs')).filter((f) => !f.includes('/docs/audit/')),
    ]
    const wrong: string[] = []
    for (const file of files) {
      const text = readFileSync(file.startsWith('/') ? file : join(ROOT, file), 'utf8')
      for (const m of text.matchAll(/\b(\d{2}) (?:web |supported )?brokers\b/gi)) {
        if (Number(m[1]) !== BROKER_COUNT) wrong.push(`${file}: ${m[0]}`)
      }
    }
    expect(wrong).toEqual([])
  })
})
