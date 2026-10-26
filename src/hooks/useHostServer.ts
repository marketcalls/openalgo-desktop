import { useEffect, useState } from 'react'

/**
 * Desktop: the address services outside this computer use to reach
 * OpenAlgo (`/api/config/host`): the Host Server URL, or the built-in
 * tunnel's while it is online. The desktop's pages are served from
 * 127.0.0.1, so `window.location.origin` is no use to TradingView; the web
 * is usually opened at its public address and reads that instead. Until
 * the answer arrives (or when it cannot be read) this computer's address
 * is shown.
 */
export function useHostServer(): string {
  const [host, setHost] = useState(() => window.location.origin)
  useEffect(() => {
    let current = true
    fetch('/api/config/host', { credentials: 'include' })
      .then((r) => (r.ok ? r.json() : null))
      .then((data: { host_server?: unknown } | null) => {
        if (current && typeof data?.host_server === 'string' && data.host_server) {
          setHost(data.host_server.replace(/\/+$/, ''))
        }
      })
      .catch(() => {})
    return () => {
      current = false
    }
  }, [])
  return host
}
