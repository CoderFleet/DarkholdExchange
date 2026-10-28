import { useCallback, useEffect, useState } from 'react'
import { getMarkets } from '../api'
import { getEvents } from '../eventsApi'

const POLL_MS = 5_000

/**
 * Loads the public events and the live market views together, and again every 5 seconds, so the
 * events list and the event room stay current (a newly published tier shows up by itself).
 *
 * `status` is 'loading' until the first answer, then 'ready', or 'error' when the last try failed.
 * After a failure the last good data stays, so a short outage does not blank the page.
 */
export function useEventData() {
  const [state, setState] = useState({
    events: [],
    views: [],
    status: 'loading',
    error: null,
  })

  const load = useCallback(async () => {
    try {
      const [events, views] = await Promise.all([getEvents(), getMarkets()])
      setState({ events, views, status: 'ready', error: null })
    } catch (error) {
      setState((current) => ({ ...current, status: 'error', error: error.message }))
    }
  }, [])

  useEffect(() => {
    const initial = window.setTimeout(load, 0)
    const poll = window.setInterval(load, POLL_MS)
    return () => {
      window.clearTimeout(initial)
      window.clearInterval(poll)
    }
  }, [load])

  return { ...state, reload: load }
}