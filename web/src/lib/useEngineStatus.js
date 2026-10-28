import { useEffect, useState } from 'react'

/**
 * Lightweight gateway liveness check for the shell. This is intentionally
 * independent of the market feed so the landing page can show a truthful
 * online/offline state before a user opens a terminal.
 *
 * @returns {'checking' | 'online' | 'offline'}
 */
export function useEngineStatus() {
  const [status, setStatus] = useState('checking')

  useEffect(() => {
    let cancelled = false
    const check = () => {
      fetch('/health')
        .then((response) => {
          if (!cancelled) setStatus(response.ok ? 'online' : 'offline')
        })
        .catch(() => {
          if (!cancelled) setStatus('offline')
        })
    }
    check()
    const timer = window.setInterval(check, 30_000)
    return () => {
      cancelled = true
      window.clearInterval(timer)
    }
  }, [])

  return status
}
