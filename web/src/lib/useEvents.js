import { useEffect, useState } from 'react'
import { getEvents } from '../eventsApi'

const POLL_MS = 15_000

/**
 * The public events (names and tier names), loaded once and then every 15 seconds. Until they
 * arrive, or if the request fails, the list is empty, so callers fall back to market names.
 */
export function useEvents() {
  const [events, setEvents] = useState([])

  useEffect(() => {
    let cancelled = false
    const load = () => {
      getEvents()
        .then((list) => {
          if (!cancelled) setEvents(list)
        })
        .catch(() => {})
    }
    const initial = window.setTimeout(load, 0)
    const poll = window.setInterval(load, POLL_MS)
    return () => {
      cancelled = true
      window.clearTimeout(initial)
      window.clearInterval(poll)
    }
  }, [])

  return events
}