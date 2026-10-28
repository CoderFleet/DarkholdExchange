import { useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { EventCard } from '../components/EventCard'
import { buildEventList, eventMatches } from '../lib/eventList'
import { useEventData } from '../lib/useEventData'
import './marketing.css'
import './events.css'

const FILTERS = [
  { id: 'all', label: 'All events' },
  { id: 'spot', label: 'Spot' },
  { id: 'escrow', label: 'Escrow' },
]

// The events page: one card per event, showing its tiers and live prices. Opening a card goes to
// the trading screen, where the event's tiers are the tabs.
export function MarketsPage() {
  const { events, views, status, error, reload } = useEventData()
  const [query, setQuery] = useState('')
  const [kind, setKind] = useState('all')

  const list = useMemo(
    () =>
      buildEventList(
        events,
        views.map((view) => view.config),
      ),
    [events, views],
  )
  const snapshots = useMemo(
    () => Object.fromEntries(views.map((view) => [view.config.id, view.snapshot])),
    [views],
  )
  const shown = useMemo(() => list.filter((event) => eventMatches(event, query, kind)), [list, query, kind])
  const tierCount = list.reduce((sum, event) => sum + event.tiers.length, 0)
  const withPrints = views.filter((view) => view.snapshot?.last_trade_price != null).length
  const loading = status === 'loading'

  return (
    <main className="markets-page">
      <header className="markets-header">
        <div>
          <span className="eyebrow">Execution books</span>
          <h1>Events</h1>
          <p>
            Open an event to trade its passes and switch between its price tiers. Every tier trades
            against a live limit order book with a server-enforced collar, integer ticks and a
            published fee.
          </p>
        </div>
        <dl className="markets-metrics">
          <div>
            <dt>Events</dt>
            <dd>{loading ? '—' : list.length}</dd>
          </div>
          <div>
            <dt>Tiers</dt>
            <dd>{loading ? '—' : tierCount}</dd>
          </div>
          <div>
            <dt>With prints</dt>
            <dd>{loading ? '—' : withPrints}</dd>
          </div>
        </dl>
      </header>

      <section className="markets-toolbar" aria-label="Filter events">
        <label className="markets-search">
          <span>Search</span>
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Event, venue, tier or ID"
          />
        </label>
        <div className="markets-filters" role="group" aria-label="Event kind">
          {FILTERS.map((filter) => (
            <button
              key={filter.id}
              type="button"
              className={kind === filter.id ? 'active' : ''}
              onClick={() => setKind(filter.id)}
            >
              {filter.label}
            </button>
          ))}
        </div>
        <span className={`markets-refresh ${status}`}>
          <span className="dot" />
          {status === 'error' ? 'Refresh failed' : 'Live · 5s refresh'}
        </span>
      </section>

      {status === 'error' && list.length === 0 ? (
        <div className="markets-empty">
          <h2>Cannot reach the matching engine</h2>
          <p>{error}</p>
          <button type="button" className="landing-button primary" onClick={reload}>
            Try again
          </button>
          <Link to="/">Back to the landing page</Link>
        </div>
      ) : loading && list.length === 0 ? (
        <div className="event-grid">
          {Array.from({ length: 3 }, (_, index) => (
            <div className="event-card skeleton" key={`skeleton-${index}`} />
          ))}
        </div>
      ) : shown.length === 0 ? (
        <div className="markets-empty">
          <h2>No events match that filter</h2>
          <p>Try a different name, venue, tier or event number.</p>
          <button
            type="button"
            className="landing-button ghost"
            onClick={() => {
              setQuery('')
              setKind('all')
            }}
          >
            Clear filters
          </button>
        </div>
      ) : (
        <div className="event-grid">
          {shown.map((event) => (
            <EventCard key={event.eventId} event={event} snapshots={snapshots} />
          ))}
        </div>
      )}

      <p className="markets-footnote">
        Prices are shown in rupees. One tick equals the market&apos;s published
        tick size; the collar is derived from face value, not set by a trader.
      </p>
    </main>
  )
}