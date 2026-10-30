/**
 * A local model provider for the Agent journey: the OpenAI chat-completions
 * stream, in the shapes of the desktop's own fake provider for Agent tests
 * (src-tauri/src/agent/testing.rs), on a port the system picks. No request
 * leaves the machine and no real model is involved.
 *
 * It answers by what it is sent: a request that offers `save_flow` and has
 * no tool result yet gets one `save_flow` call with the scripted workflow;
 * a request carrying the tool's result gets a closing sentence; anything else
 * (the credential test) gets a short text.
 */

import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http'
import type { AddressInfo } from 'node:net'

function chunk(delta: unknown, finish: string | null): string {
  const v = {
    id: 'chatcmpl-1',
    object: 'chat.completion.chunk',
    created: 1,
    model: 'gpt-fake',
    choices: [{ index: 0, delta, finish_reason: finish }],
  }
  return `data: ${JSON.stringify(v)}\n\n`
}

function usage(): string {
  const v = {
    id: 'chatcmpl-1',
    object: 'chat.completion.chunk',
    created: 1,
    model: 'gpt-fake',
    choices: [],
    usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110 },
  }
  return `data: ${JSON.stringify(v)}\n\ndata: [DONE]\n\n`
}

export function textStream(text: string): string {
  return chunk({ role: 'assistant', content: text }, null) + chunk({}, 'stop') + usage()
}

export function toolCallStream(id: string, name: string, args: string): string {
  const mid = Math.floor(args.length / 2)
  return (
    chunk(
      { tool_calls: [{ index: 0, id, type: 'function', function: { name, arguments: '' } }] },
      null
    ) +
    chunk({ tool_calls: [{ index: 0, function: { arguments: args.slice(0, mid) } }] }, null) +
    chunk({ tool_calls: [{ index: 0, function: { arguments: args.slice(mid) } }] }, null) +
    chunk({}, 'tool_calls') +
    usage()
  )
}

export interface FakeProvider {
  endpoint: string
  requests: any[]
  close(): Promise<void>
}

/** Starts the provider; `saveArgs` is the save_flow call it makes. */
export async function startFakeProvider(saveArgs: Record<string, unknown>): Promise<FakeProvider> {
  const requests: any[] = []
  const answer = (body: any): string => {
    const messages: any[] = body?.messages ?? []
    if (messages.some((m) => m.role === 'tool')) {
      return textStream('Saved. It is inactive: open Flow to review and activate it.')
    }
    const offered = (body?.tools ?? []).some((t: any) => t?.function?.name === 'save_flow')
    if (offered) return toolCallStream('call_1', 'save_flow', JSON.stringify(saveArgs))
    return textStream('p')
  }
  const server: Server = createServer((req: IncomingMessage, res: ServerResponse) => {
    const parts: Buffer[] = []
    req.on('data', (c: Buffer) => parts.push(c))
    req.on('end', () => {
      let body: any = null
      try {
        body = JSON.parse(Buffer.concat(parts).toString('utf8'))
      } catch {
        body = null
      }
      requests.push({ path: req.url, body })
      const payload = answer(body)
      res.writeHead(200, {
        'Content-Type': 'text/event-stream',
        Connection: 'close',
        'Content-Length': Buffer.byteLength(payload),
      })
      res.end(payload)
    })
  })
  await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve))
  const { port } = server.address() as AddressInfo
  return {
    endpoint: `http://127.0.0.1:${port}/v1/`,
    requests,
    close: () =>
      new Promise<void>((resolve) => {
        server.closeAllConnections()
        server.close(() => resolve())
      }),
  }
}
