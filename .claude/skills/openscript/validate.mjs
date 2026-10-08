#!/usr/bin/env node
/**
 * Compile one OpenScript file the way the browser compiles it, and install it
 * only if it came out clean.
 *
 * ## Why this exists rather than writing the file straight in
 *
 * The app's `openscript/` folder (in its app data directory) holds two files per script: the source a trader
 * reads, and `<name>.oscript.program.json` beside it, which is the compiled
 * program. **The runner reads only the program.** It never opens the source.
 *
 * That makes two silent failures possible, and both of them matter more than a
 * compile error:
 *
 * - **A source with no program is a strategy that cannot run.** It saves, it
 *   opens in the editor, it appears in the list, and starting it is refused a
 *   minute later in a log with "has no compiled program yet".
 * - **A source whose program was built from different text is worse.** A
 *   program records the hash of the source it came from, so the platform
 *   refuses the pair on save; but a pair written past that route would leave a
 *   run executing one script while a trader reads another.
 *
 * So the two files are written together, from one compile, or neither is.
 *
 * ## What a pass means and what it does not
 *
 * A pass means the compiler accepted the text and emitted a program: the names
 * resolve, the types agree, the calls exist, the limits hold. It does not mean
 * the strategy is any good, that its numbers are right, or that it will make
 * money. Nothing here runs a single bar.
 *
 * Usage:
 *   node .claude/skills/openscript/validate.mjs <draft.oscript>
 *   node .claude/skills/openscript/validate.mjs <draft.oscript> --install --into appdata
 *   node .claude/skills/openscript/validate.mjs <draft.oscript> --install --into <dir> --as my-name.oscript
 */

import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { basename, join, resolve, sep } from 'node:path'
import { pathToFileURL } from 'node:url'

const REPO_ROOT = resolve(import.meta.dirname, '..', '..', '..')

/**
 * Desktop: the app's node_modules sits at the repository root. In a worktree
 * under .claude/worktrees/ that has no node_modules of its own, the main
 * checkout's copy is used (same package.json pin, so the same version).
 */
function devPackage(repo, name) {
  const own = join(repo, 'node_modules', name)
  if (existsSync(own)) return own
  const marker = `${sep}.claude${sep}worktrees${sep}`
  const at = repo.indexOf(marker)
  return at < 0 ? own : join(repo.slice(0, at), 'node_modules', name)
}

/** A front end developer already has it here, at the exact pinned version. */
const DEV_ROOT = devPackage(REPO_ROOT, 'openalgo-script')
/** Everyone else gets just this one package cached here. Gitignored. */
const CACHE_ROOT = join(import.meta.dirname, '.cache')

/** What the platform will accept as a script name. Restated from the route. */
const SAFE_NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}\.oscript$/
/** What the compiled program is called, beside its source. */
const PROGRAM_SUFFIX = '.program.json'

// ---------------------------------------------------------------------------
// Finding the compiler, without making anyone run `npm install`
// ---------------------------------------------------------------------------
//
// The full frontend tree is hundreds of megabytes. This needs one package with
// zero dependencies. OpenAlgo users are traders, not front end developers, and
// a script compiles in the browser with no build step: asking them to build one
// to write a script would be the wrong shape entirely.
//
// The version comes from `package.json`, so this cannot quietly
// compile against an API the running app does not have.

function pinnedVersion() {
  try {
    const pkg = JSON.parse(readFileSync(join(REPO_ROOT, 'package.json'), 'utf8'))
    const spec = pkg.dependencies?.['openalgo-script'] ?? pkg.devDependencies?.['openalgo-script']
    return typeof spec === 'string' ? spec.replace(/^[\^~]/, '') : null
  } catch {
    return null
  }
}

function installedVersion(root) {
  try {
    return JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version ?? null
  } catch {
    return null
  }
}

function entryPoint(root) {
  return join(root, 'dist', 'core', 'index.js')
}

function fetchPackage(version, prefix) {
  mkdirSync(prefix, { recursive: true })
  const spec = version ? `openalgo-script@${version}` : 'openalgo-script'
  // npm is a .cmd on Windows and Node will not spawn one without a shell. With
  // a shell nothing is auto-quoted, so the paths are quoted here or a checkout
  // under a directory with a space in it splits into two arguments.
  const win = process.platform === 'win32'
  const q = (s) => (win ? `"${s}"` : s)
  execFileSync(
    win ? 'npm.cmd' : 'npm',
    ['install', q(spec), '--prefix', q(prefix), '--no-save', '--no-package-lock',
     '--no-audit', '--no-fund', '--loglevel', 'error'],
    { stdio: ['ignore', 'ignore', 'pipe'], shell: win }
  )
}

function resolveCompiler() {
  const want = pinnedVersion()

  if (existsSync(entryPoint(DEV_ROOT))) {
    return { root: DEV_ROOT, source: 'node_modules' }
  }

  const prefix = join(CACHE_ROOT, `v${want ?? 'latest'}`)
  const cached = join(prefix, 'node_modules', 'openalgo-script')
  if (existsSync(entryPoint(cached))) {
    const have = installedVersion(cached)
    if (!want || have === want) return { root: cached, source: 'skill cache' }
  }

  console.log(`Fetching openalgo-script@${want ?? 'latest'} (one package, no dependencies)...`)
  fetchPackage(want, prefix)
  if (!existsSync(entryPoint(cached))) {
    throw new Error('the fetched package has no dist/core/index.js')
  }
  return { root: cached, source: 'downloaded' }
}


/**
 * Desktop: where an install goes. There is no repository folder the app reads
 * from; the app serves the trader's files from its data directory, which is
 * Tauri's app data dir for the identifier `com.openalgo.desktop`:
 *
 *   macOS    ~/Library/Application Support/com.openalgo.desktop/openscript
 *   Windows  %APPDATA%\com.openalgo.desktop\openscript
 *   Linux    ${XDG_DATA_HOME:-~/.local/share}/com.openalgo.desktop/openscript
 *
 * The destination is never implied: `--into <dir>` names a folder, and
 * `--into appdata` asks for the app's own folder above. The app's data
 * directory must already exist (the app has run once); this script creates
 * only the last folder, never the app's data directory itself.
 */
function appDataDir() {
  const home = homedir()
  if (process.platform === 'darwin') {
    return join(home, 'Library', 'Application Support', 'com.openalgo.desktop')
  }
  if (process.platform === 'win32') {
    return join(process.env.APPDATA ?? join(home, 'AppData', 'Roaming'), 'com.openalgo.desktop')
  }
  return join(process.env.XDG_DATA_HOME || join(home, '.local', 'share'), 'com.openalgo.desktop')
}

function resolveInstallDir(args, kind) {
  const at = args.indexOf('--into')
  if (at === -1) return { dir: null, flagAt: -1 }
  const value = args[at + 1]
  if (!value || value.startsWith('--')) {
    console.error(`--into needs a folder, or 'appdata' for ${join(appDataDir(), kind)}`)
    process.exit(2)
  }
  if (value === 'appdata') {
    const base = appDataDir()
    if (!existsSync(base)) {
      console.error(
        `The app data folder ${base} does not exist. Start OpenAlgo Desktop once, or pass --into <dir>.`
      )
      process.exit(2)
    }
    return { dir: join(base, kind), flagAt: at }
  }
  return { dir: resolve(value), flagAt: at }
}

// ---------------------------------------------------------------------------

function usage(message) {
  console.error(message)
  console.error('')
  console.error('  node .claude/skills/openscript/validate.mjs <draft.oscript>')
  console.error('  node .claude/skills/openscript/validate.mjs <draft.oscript> --install --into appdata')
  console.error('  node .claude/skills/openscript/validate.mjs <draft.oscript> --install --into <dir> --as name.oscript')
  process.exit(2)
}

const argv = process.argv.slice(2)
const install = argv.includes('--install')
const asAt = argv.indexOf('--as')
const asName = asAt === -1 ? null : argv[asAt + 1]
// The value after --as is that flag's, not the draft. Guarded on asAt being
// found: without the guard, an absent --as makes this skip argument zero, which
// is the draft in the ordinary call.
const into = resolveInstallDir(argv, 'openscript')
const INSTALL_DIR = into.dir
const draft = argv.find(
  (one, at) =>
    !one.startsWith('--') &&
    !(asAt !== -1 && at === asAt + 1) &&
    !(into.flagAt !== -1 && at === into.flagAt + 1)
)

if (!draft) usage('Give the draft file to compile.')
if (!existsSync(draft)) usage(`No such file: ${draft}`)
if (asAt !== -1 && !asName) usage('--as needs a file name.')
if (install && !INSTALL_DIR) {
  usage(`--install needs --into <dir>, or --into appdata for ${join(appDataDir(), 'openscript')}`)
}

const target = asName ?? basename(draft)
if (install && !SAFE_NAME.test(target)) {
  usage(
    `${JSON.stringify(target)} is not a name this platform will store.\n` +
      'A name is letters, digits, dot, dash or underscore, starts with a letter or a digit, ' +
      'is at most 64 characters before the extension, and ends in .oscript'
  )
}

const drafted = readFileSync(draft, 'utf8')

let compiler
try {
  compiler = resolveCompiler()
} catch (cause) {
  console.error('Could not get the compiler this app ships.')
  console.error(`  ${cause.message}`)
  console.error('')
  console.error('Either connect to the network once so the one package can be cached, or say')
  console.error('so and write the script through the editor at /trading, which compiles it in')
  console.error('the browser. Do not install a script that has not been compiled: the runner')
  console.error('reads the compiled program and never the source.')
  process.exit(1)
}

const engine = await import(pathToFileURL(entryPoint(compiler.root)).href)
const version = installedVersion(compiler.root) ?? 'unknown'

// **One text, from here down.** The compiler normalises line endings before it
// reads anything, and the hash it records is of the normalised text. A draft
// written with CRLF, which is the ordinary thing on Windows, would then hash
// differently on disk from what its own program records, and the platform
// refuses that pair on save. Normalising once here means the bytes installed
// are the bytes the program is about.
const source = engine.normaliseSource(drafted)

// The same five calls, in the same order, that the editor makes. Anything else
// would be validating against a compiler the app does not run.
const handle = engine.sourceFile(target, source)
const bag = new engine.DiagnosticBag()
const tokens = engine.lex(handle, bag)
const tree = engine.parseTokens(handle, tokens, bag)
const checked = engine.check(handle, tree, bag)
const emitted = engine.emit(handle, checked, bag, {})

const said = bag.ordered()
const errors = said.filter((one) => one.severity === 'error')
const warnings = said.filter((one) => one.severity !== 'error')
const lines = source.split('\n')

function report(one) {
  const line = one.span?.line ?? 1
  const column = one.span?.column ?? 1
  const text = lines[line - 1] ?? ''
  console.log(`  ${one.code}  line ${line}, column ${column}: ${one.message}`)
  if (text.trim() !== '') {
    console.log(`      ${text}`)
    console.log(`      ${' '.repeat(Math.max(0, column - 1))}${'^'.repeat(Math.max(1, one.span?.length ?? 1))}`)
  }
  if (one.fix) console.log(`      fix: ${one.fix}`)
}

console.log(`openalgo-script ${version} (${compiler.source})`)
console.log(`Compiling ${draft} as ${target}`)
console.log('')

if (errors.length > 0) {
  console.log(`FAILED: ${errors.length} error${errors.length === 1 ? '' : 's'}`)
  for (const one of errors) report(one)
  if (warnings.length > 0) {
    console.log('')
    console.log(`and ${warnings.length} warning${warnings.length === 1 ? '' : 's'}:`)
    for (const one of warnings) report(one)
  }
  console.log('')
  console.log('Nothing was installed. Fix the draft and run again. Do not weaken a check to')
  console.log('get a pass: every code above names what is wrong and what to do about it.')
  process.exit(1)
}

if (emitted.program === undefined) {
  console.log('FAILED: the compiler raised no error and produced no program.')
  console.log('That is a fault in the compiler rather than in this script. Please report it.')
  process.exit(1)
}

const program = emitted.program
const kind = program.meta?.kind ?? 'unknown'
const requires = program.requires ?? []
const plots = program.outputs?.plots?.length ?? 0
const inputs = program.inputs?.length ?? 0

console.log(`PASSED: compiles as a ${kind}`)
console.log(`  declares   : ${program.meta?.title ?? '(no title)'}`)
console.log(`  requires   : ${requires.join(', ') || '(nothing beyond the core)'}`)
console.log(`  plots      : ${plots}`)
console.log(`  inputs     : ${inputs}`)
if (kind === 'strategy') {
  const declared = program.meta?.strategy ?? {}
  console.log(`  quantity   : ${JSON.stringify(declared.qty)} (${declared.qtyType ?? 'units'})`)
  console.log(`  product    : ${declared.product ?? '(none declared)'}`)
}

if (warnings.length > 0) {
  console.log('')
  console.log(`${warnings.length} warning${warnings.length === 1 ? '' : 's'}, which do not stop it running:`)
  for (const one of warnings) report(one)
  console.log('')
  console.log('Tell the person who asked for this about them rather than passing over them.')
}

// A strategy that places orders is a strategy that spends money. Say so once,
// here, where somebody is about to install one.
if (requires.includes('orders')) {
  console.log('')
  console.log('This places orders. Installed, it still does nothing until somebody deploys it')
  console.log('on an instrument and starts it, and where its orders go is the platform mode:')
  console.log('live with a broker, or the sandbox in analyzer mode.')
}

if (!install) {
  console.log('')
  console.log('Not installed. Re-run with --install --into appdata (the app folder')
  console.log(`${join(appDataDir(), 'openscript')}) or --into <dir> to write it and its compiled program.`)
  process.exit(0)
}

// ---------------------------------------------------------------------------
// Install: both files, from this one compile, or neither
// ---------------------------------------------------------------------------

const canonical = engine.canonicalise(program)

// Read back what was just written and check the pair agrees, the way the
// platform's own save route checks it: a program carries the hash of the source
// it was compiled from, and a mismatched pair would leave a run executing one
// script while a trader reads another.
const recorded = JSON.parse(canonical).source?.hash
const expected = engine.sourceHash(source)
if (recorded !== expected) {
  console.log('')
  console.log('FAILED: the compiled program does not record this source.')
  console.log(`  program says: ${recorded}`)
  console.log(`  source is   : ${expected}`)
  console.log('Nothing was installed.')
  process.exit(1)
}

mkdirSync(INSTALL_DIR, { recursive: true })
const sourceAt = join(INSTALL_DIR, target)
const programAt = join(INSTALL_DIR, target + PROGRAM_SUFFIX)
const replacing = existsSync(sourceAt)

writeFileSync(sourceAt, source, 'utf8')
writeFileSync(programAt, canonical, 'utf8')

console.log('')
console.log(`${replacing ? 'Replaced' : 'Installed'}:`)
console.log(`  ${sourceAt}`)
console.log(`  ${programAt}`)
console.log('')
console.log('It is in the editor at /trading now. A study can be added to a chart from the')
console.log('indicator list; a strategy is deployed on an instrument from the Strategies')
console.log('panel, which is what decides the instrument, the timeframe and the product.')
