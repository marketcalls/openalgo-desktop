/**
 * Starts the app's real server (`src-tauri/examples/e2e_server.rs`) for the
 * Flow journeys and creates the trader's account; each journey signs in to
 * it fresh (support/signin.ts: logged in to the scripted broker, the Dhan
 * (Sandbox) slot of CI's native test build, analyzer mode on), so a run that
 * spans 03:00 IST does not meet a signed-out session. The returned function
 * is the global teardown: it stops the server and removes its folder.
 *
 * The server never outlives the run: its standard input is a pipe from this
 * process, and it exits when that pipe closes, so a runner that crashes or is
 * killed takes it down too. The teardown closes the pipe, then sends SIGTERM,
 * then SIGKILL, and always waits for the exit; a last `exit` hook kills it if
 * the runner leaves another way.
 */

import { type ChildProcess, spawn } from 'node:child_process'
import {
  createWriteStream,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { createInterface } from 'node:readline'
import { fileURLToPath } from 'node:url'
import { request } from '@playwright/test'
import { clearOfBoundary, postForm, signIn, type Trader } from './signin'

const PREFIX = 'openalgo-flow-e2e-'
const READY = /^OPENALGO_E2E_SERVER (http:\/\/127\.0\.0\.1:\d+)\s*$/
const START_DEADLINE_MS = 120_000
const ROOT = fileURLToPath(new URL('../../..', import.meta.url))

function serverBinary(): string {
  const given = process.env.OPENALGO_E2E_SERVER_BIN
  const bin = given ?? path.join(ROOT, 'src-tauri', 'target', 'debug', 'examples', 'e2e_server')
  const exe = process.platform === 'win32' && !bin.endsWith('.exe') ? `${bin}.exe` : bin
  if (!existsSync(exe)) {
    throw new Error(
      `The e2e server is not built (${exe}). From src-tauri: cargo build --example e2e_server --features e2e-broker`
    )
  }
  return exe
}

/** An environment with nothing of this machine's app in it: a fresh home. */
function freshEnv(home: string): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { PATH: process.env.PATH ?? '', LANG: 'C.UTF-8' }
  if (process.env.SystemRoot) env.SystemRoot = process.env.SystemRoot
  for (const name of ['HOME', 'XDG_DATA_HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'TMPDIR']) {
    const dir = path.join(home, name.toLowerCase())
    mkdirSync(dir, { recursive: true })
    env[name] = dir
  }
  env.APPDATA = env.XDG_DATA_HOME
  env.LOCALAPPDATA = env.XDG_CACHE_HOME
  return env
}

function exited(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve()
  return new Promise((resolve) => child.once('exit', () => resolve()))
}

async function within(p: Promise<void>, ms: number): Promise<boolean> {
  let timer: NodeJS.Timeout | undefined
  const late = new Promise<boolean>((resolve) => {
    timer = setTimeout(() => resolve(false), ms)
  })
  const done = await Promise.race([p.then(() => true), late])
  clearTimeout(timer)
  return done
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0)
    return true
  } catch {
    return false
  }
}

/**
 * The folders of earlier runs whose runner was killed before its teardown
 * (the server itself exits when its pipe closes): removed once their server
 * is gone. Only this harness's own prefix, and only with a dead server.
 */
function sweepLeftovers(): void {
  for (const name of readdirSync(os.tmpdir())) {
    if (!name.startsWith(PREFIX)) continue
    const folder = path.join(os.tmpdir(), name)
    try {
      const pid = Number(readFileSync(path.join(folder, 'server.pid'), 'utf8'))
      if (pid > 0 && !alive(pid)) rmSync(folder, { recursive: true, force: true })
    } catch {
      // No pid file yet (a run starting now) or unreadable: left alone.
    }
  }
}

/** Close the pipe, then SIGTERM, then SIGKILL; resolves once it has exited. */
async function stop(child: ChildProcess): Promise<void> {
  const gone = exited(child)
  child.stdin?.end()
  if (await within(gone, 20_000)) return
  child.kill('SIGTERM')
  if (await within(gone, 5_000)) return
  child.kill('SIGKILL')
  await gone
}

/** Waits for the ready line; fails if the server exits or takes too long. */
function waitReady(child: ChildProcess, logPath: string): Promise<string> {
  return new Promise((resolve, reject) => {
    const lines = createInterface({ input: child.stdout! })
    const timer = setTimeout(
      () => reject(new Error(`The e2e server did not start within 120 s (log: ${logPath})`)),
      START_DEADLINE_MS
    )
    lines.on('line', (line) => {
      const m = READY.exec(line)
      if (m) {
        clearTimeout(timer)
        resolve(m[1])
      }
    })
    child.once('exit', (code, signal) => {
      clearTimeout(timer)
      reject(
        new Error(`The e2e server exited (${code ?? signal}) before it listened (log: ${logPath})`)
      )
    })
  })
}

/**
 * The account every journey signs in to: created on the setup page, then
 * signed in once (broker login, analyzer mode) so a broken start fails here
 * rather than in the first journey. Each journey signs in again itself
 * (support/signin.ts).
 */
async function createTrader(base: string): Promise<Trader> {
  const trader: Trader = {
    username: 'trader',
    password: `Aa1!${Math.random().toString(36).slice(2)}${Date.now().toString(36)}`,
  }
  const ctx = await request.newContext({ baseURL: base })
  try {
    await postForm(ctx, '/setup', {
      username: trader.username,
      email: 'trader@example.com',
      password: trader.password,
      confirm_password: trader.password,
    })
  } finally {
    await ctx.dispose()
  }
  await clearOfBoundary()
  await signIn(base, trader)
  return trader
}

export default async function globalSetup(): Promise<() => Promise<void>> {
  const bin = serverBinary()
  sweepLeftovers()
  const folder = mkdtempSync(path.join(os.tmpdir(), PREFIX))
  const data = path.join(folder, 'data')
  mkdirSync(data)
  const logDir = path.join(ROOT, 'test-results', 'flow-server')
  mkdirSync(logDir, { recursive: true })
  const logPath = path.join(logDir, 'server.log')
  const log = createWriteStream(logPath)

  const child = spawn(bin, ['--data', data], {
    env: freshEnv(path.join(folder, 'home')),
    stdio: ['pipe', 'pipe', 'pipe'],
  })
  if (child.pid) writeFileSync(path.join(folder, 'server.pid'), String(child.pid))
  const lastResort = () => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL')
  }
  process.once('exit', lastResort)
  child.stderr!.pipe(log)

  const teardown = async () => {
    await stop(child)
    process.removeListener('exit', lastResort)
    log.end()
    rmSync(folder, { recursive: true, force: true })
  }

  try {
    const base = await waitReady(child, logPath)
    const trader = await createTrader(base)
    process.env.OPENALGO_E2E_BASE = base
    // For the workers' own sign-ins; a throwaway account on a throwaway server.
    process.env.OPENALGO_E2E_TRADER = JSON.stringify(trader)
    process.env.OPENALGO_E2E_SERVER_LOG = logPath
  } catch (e) {
    await teardown()
    throw e
  }
  return teardown
}
