/**
 * Desktop: access tokens for AI clients (MCP), shown on the API key page.
 *
 * The web connects hosted AI clients through OAuth; the desktop's first
 * version uses a token created here instead. A token is shown once, with the
 * ready-to-paste configuration for Claude Desktop and Claude Code. All calls
 * go through src/lib/desktop.ts.
 */

import { useCallback, useEffect, useState } from 'react'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  createMcpToken,
  fetchMcpTokens,
  type McpClientConfig,
  type McpToken,
  revokeMcpToken,
} from '@/lib/desktop'
import { showToast } from '@/utils/toast'

const SCOPE_LABEL: Record<McpToken['scope'], string> = {
  read: 'Read only',
  read_write: 'Read and place orders',
}

async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text)
    showToast.success('Copied to clipboard')
  } catch {
    showToast.error('Copy failed. Select the text and copy it manually.')
  }
}

export function McpTokens() {
  const [tokens, setTokens] = useState<McpToken[]>([])
  const [name, setName] = useState('Claude')
  const [scope, setScope] = useState<McpToken['scope']>('read')
  const [busy, setBusy] = useState(false)
  const [created, setCreated] = useState<{ token: string; config: McpClientConfig } | null>(null)

  const load = useCallback(async () => {
    setTokens(await fetchMcpTokens())
  }, [])

  useEffect(() => {
    void load()
  }, [load])

  const handleCreate = async () => {
    setBusy(true)
    try {
      const res = await createMcpToken(name.trim() || 'AI client', scope)
      setCreated({ token: res.token, config: res.client_config })
      await load()
    } catch (e) {
      showToast.error(e instanceof Error ? e.message : 'Could not create the token. Try again.')
    } finally {
      setBusy(false)
    }
  }

  const handleRevoke = async (id: number) => {
    try {
      await revokeMcpToken(id)
      showToast.success('Token revoked. AI clients using it are disconnected.')
      await load()
    } catch (e) {
      showToast.error(e instanceof Error ? e.message : 'Could not revoke the token. Try again.')
    }
  }

  const desktopConfig = created ? JSON.stringify(created.config.claude_desktop, null, 2) : ''

  return (
    <Card className="lg:col-span-2">
      <CardHeader>
        <CardTitle>AI clients (MCP)</CardTitle>
        <CardDescription>
          Let Claude Desktop, Claude Code and other AI assistants read your account and market data,
          and if you allow it, place orders. Create a token for each client. Orders follow the
          analyzer mode switch, exactly like the API.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-6">
        <div className="grid gap-4 sm:grid-cols-[1fr_auto_auto] sm:items-end">
          <div className="space-y-2">
            <Label htmlFor="mcp-token-name">Client name</Label>
            <Input
              id="mcp-token-name"
              value={name}
              maxLength={64}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="mcp-token-scope">Access</Label>
            <select
              id="mcp-token-scope"
              className="h-9 rounded-md border bg-background px-3 text-sm"
              value={scope}
              onChange={(e) => setScope(e.target.value as McpToken['scope'])}
            >
              <option value="read">{SCOPE_LABEL.read}</option>
              <option value="read_write">{SCOPE_LABEL.read_write}</option>
            </select>
          </div>
          <Button onClick={handleCreate} disabled={busy}>
            Create token
          </Button>
        </div>

        {created ? (
          <div className="space-y-3 rounded-lg border p-4">
            <p className="text-sm font-medium">Copy this token now. It is shown only once.</p>
            <div className="flex items-center gap-2">
              <code className="flex-1 break-all rounded bg-muted p-2 font-mono text-xs">
                {created.token}
              </code>
              <Button size="sm" variant="outline" onClick={() => copy(created.token)}>
                Copy
              </Button>
            </div>
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-sm font-medium">
                  Claude Desktop (claude_desktop_config.json)
                </span>
                <Button size="sm" variant="ghost" onClick={() => copy(desktopConfig)}>
                  Copy
                </Button>
              </div>
              <pre className="overflow-x-auto rounded bg-muted p-2 text-xs">{desktopConfig}</pre>
            </div>
            <div className="space-y-1">
              <div className="flex items-center justify-between">
                <span className="text-sm font-medium">Claude Code</span>
                <Button size="sm" variant="ghost" onClick={() => copy(created.config.claude_code)}>
                  Copy
                </Button>
              </div>
              <pre className="overflow-x-auto rounded bg-muted p-2 text-xs">
                {created.config.claude_code}
              </pre>
            </div>
            <Button size="sm" variant="outline" onClick={() => setCreated(null)}>
              Done
            </Button>
          </div>
        ) : null}

        <div className="space-y-2">
          <h3 className="text-sm font-semibold">Active tokens</h3>
          {tokens.length === 0 ? (
            <p className="text-sm text-muted-foreground">No AI client tokens yet.</p>
          ) : (
            <ul className="divide-y rounded-lg border">
              {tokens.map((t) => (
                <li key={t.id} className="flex flex-wrap items-center gap-3 p-3 text-sm">
                  <span className="font-medium">{t.name}</span>
                  <span className="text-muted-foreground">{SCOPE_LABEL[t.scope]}</span>
                  <code className="font-mono text-xs text-muted-foreground">
                    {t.token_prefix}...
                  </code>
                  <span className="text-xs text-muted-foreground">
                    {t.last_used_at ? `Last used ${t.last_used_at} UTC` : 'Not used yet'}
                  </span>
                  <Button
                    size="sm"
                    variant="outline"
                    className="ml-auto"
                    onClick={() => handleRevoke(t.id)}
                  >
                    Revoke
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </div>
      </CardContent>
    </Card>
  )
}
