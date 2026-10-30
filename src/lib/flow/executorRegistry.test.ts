/**
 * The run can execute exactly the node types the validator accepts (parity
 * F-697): the executor's registry (src-tauri/src/flow/nodes/mod.rs
 * FAMILIES, every family's build list) plus the three gates the walk
 * evaluates itself (contract/model.rs GATE_TYPES, used by
 * executor/walk.rs) equal validator/catalog.rs VALID_NODE_TYPES, with no
 * type registered twice and no gate registered as a node. NodePalette.test
 * ties the same validator list to the palette and the canvas, so the four
 * agree. The Rust sources are read as written, the way the palette test
 * reads the catalog; a family, constructor or node type this reader cannot
 * resolve fails the test rather than being skipped.
 */

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'

const FLOW = join(process.cwd(), 'src-tauri/src/flow')
const NODES = join(FLOW, 'nodes')

/** A Rust source without its comments. */
function source(path: string): string {
  return readFileSync(path, 'utf8')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/\/\/[^\n]*/g, '')
}

/** Every non-test source file under the nodes folder. */
function nodeSources(dir = NODES): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name)
    if (statSync(path).isDirectory()) return /test/.test(name) ? [] : nodeSources(path)
    return name.endsWith('.rs') && !/test/.test(name) ? [source(path)] : []
  })
}

const ALL = nodeSources().join('\n')

/** The text between the bracket at `open` and its match. */
function balanced(text: string, open: number): string {
  const pairs: Record<string, string> = { '{': '}', '(': ')', '[': ']' }
  const close = pairs[text[open]]
  let depth = 0
  for (let i = open; i < text.length; i++) {
    if (text[i] === text[open]) depth++
    if (text[i] === close && --depth === 0) return text.slice(open + 1, i)
  }
  throw new Error(`unbalanced ${text[open]} at ${open}`)
}

/** The block after the first match of `head`, whose last character opens it. */
function blockAfter(text: string, head: RegExp, what: string): string {
  const m = head.exec(text)
  if (!m) throw new Error(`no ${what}`)
  return balanced(text, m.index + m[0].length - 1)
}

/** The modules FAMILIES lists, in order. */
function familyModules(): string[] {
  const list = blockAfter(
    source(join(NODES, 'mod.rs')),
    /pub const FAMILIES: &\[NodeFamily\] = &\[/,
    'FAMILIES'
  )
  return [...list.matchAll(/(\w+)::FAMILY/g)].map((m) => m[1])
}

/** The constructor expressions a family's build list makes, `Arc::new(<this>)`. */
function constructors(module: string): string[] {
  const file = [join(NODES, `${module}.rs`), join(NODES, module, 'mod.rs')].find((p) => {
    try {
      return statSync(p).isFile()
    } catch {
      return false
    }
  })
  if (!file) throw new Error(`no source for family ${module}`)
  const family = blockAfter(
    source(file),
    /pub const FAMILY: NodeFamily = NodeFamily \{/,
    `FAMILY in ${module}`
  )
  const build = family.slice(family.indexOf('build:'))
  const out: string[] = []
  for (const m of build.matchAll(/Arc::new\(/g)) {
    out.push(balanced(build, (m.index ?? 0) + m[0].length - 1).trim())
  }
  if (out.length === 0) throw new Error(`family ${module} builds nothing`)
  return out
}

/** The node type a constructor expression registers. */
function nodeType(ctor: string): string {
  const field = ctor.match(/node_type:\s*"(\w+)"/)
  if (field) return field[1]
  const name = ctor.match(/^(?:\w+::)*(\w+)/)?.[1]
  const args = ctor.match(/^[\w:]+\((.*)\)$/s)?.[1]?.trim() ?? ''
  const impl = blockAfter(
    ALL,
    new RegExp(`impl FlowNode for ${name}\\s*\\{`),
    `impl FlowNode for ${name}`
  )
  const body = blockAfter(
    impl,
    /fn node_type\(&self\) -> &'static str \{/,
    `node_type in ${name}`
  ).trim()
  const literal = body.match(/^"(\w+)"$/)
  if (literal) return literal[1]
  if (body === 'self.0.node_type') {
    // A node built from a static, `Subscribe(&LTP)`.
    const name = args.replace(/^&/, '')
    const value = blockAfter(
      ALL,
      new RegExp(`(?:static|const) ${name}: \\w+ = \\w+ \\{`),
      `static ${name}`
    )
    const typed = value.match(/node_type:\s*"(\w+)"/)
    if (typed) return typed[1]
  }
  if (body === 'self.words().0') {
    // A node built from an enum variant, `BookNode(Book::Orders)`.
    const typed = ALL.match(new RegExp(`${args}\\s*=>\\s*\\(\\s*"(\\w+)"`))
    if (typed) return typed[1]
  }
  throw new Error(`cannot read the node type of ${ctor}`)
}

/** A string list constant's strings (`&[&str]` or `[&str; N]`). */
function strings(file: string, name: string): string[] {
  const list = blockAfter(
    source(file),
    new RegExp(`pub const ${name}: &?\\[&str(?:; \\d+)?\\] = &?\\[`),
    name
  )
  return [...list.matchAll(/"(\w+)"/g)].map((m) => m[1])
}

const registered = familyModules().flatMap((module) => constructors(module).map(nodeType))
const gates = strings(join(FLOW, 'contract/model.rs'), 'GATE_TYPES')
const validator = strings(join(FLOW, 'validator/catalog.rs'), 'VALID_NODE_TYPES')

describe('the run executes exactly the node types the validator accepts (F-697)', () => {
  it('reads the seven families the registry lists', () => {
    expect(familyModules()).toEqual([
      'core',
      'logic',
      'data',
      'orders',
      'options',
      'triggers',
      'streaming',
    ])
  })

  it('registers each node type once, 58 in all', () => {
    expect(new Set(registered).size).toBe(registered.length)
    expect(registered).toHaveLength(58)
  })

  it('leaves the three gates to the walk, which evaluates them itself', () => {
    expect(gates).toEqual(['andGate', 'orGate', 'notGate'])
    expect(registered.filter((t) => gates.includes(t))).toEqual([])
    expect(source(join(FLOW, 'executor/walk.rs'))).toMatch(/GATE_TYPES\.contains\(&node_type\)/)
  })

  it('equals VALID_NODE_TYPES with the gates, 61 types', () => {
    expect([...registered, ...gates].sort()).toEqual([...validator].sort())
    expect(validator).toHaveLength(61)
  })
})
