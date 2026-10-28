import { useCallback, useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { getMarkets } from '../api'
import { MarketCard } from '../components/MarketCard'
import { filterMarketViews, marketOverview } from '../lib/marketOverview'
import './marketing.css'

const POLL_MS = 5_000

const FILTERS = [
  { id: 'all', label: 'All markets' },
  { id: 'spot', label: 'Spot' },
  { id: 'escrow', label: 'Escrow' },
]

export function MarketsPage() {
  const [views, setViews] = useState([])
  const [status, setStatus] = useState('loading')
  const [error, setError] = useState(null)
  const [query, setQuery] = useState('')
  const [kind, setKind] = useState('all')

  const load = useCallback(async () => {
    try {
      const next = await getMarkets()
      setViews(next)
      setStatus('ready')
      setError(null)
    } catch (nextError) {
      setStatus('error')
      setError(nextError.message)
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

  const overview = useMemo(() => marketOverview(views), [views])
  const visible = useMemo(
    () => filterMarketViews(views, { query, kind }),
    [views, query, kind],
  )

  return (
    <main className="markets-page">
      <header className="markets-header">
        <div>
          <span className="eyebrow">Execution books</span>
          <h1>Markets</h1>
          <p>
            Every event pass trades against a live limit order book with a
            server-enforced collar, integer ticks and a published fee.
          </p>
        </div>
        <dl className="markets-metrics">
          <div>
            <dt>Live books</dt>
            <dd>{status === 'loading' ? '—' : overview.total}</dd>
          </div>
          <div>
            <dt>Spot / escrow</dt>
            <dd>
              {status === 'loading' ? '—' : `${overview.spot} / ${overview.escrow}`}
            </dd>
          </div>
          <div>
            <dt>With prints</dt>
            <dd>{status === 'loading' ? '—' : overview.withPrints}</dd>
          </div>
        </dl>
      </header>

      <section className="markets-toolbar" aria-label="Filter markets">
        <label className="markets-search">
          <span>Search</span>
          <input
            type="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Market, ID or event"
          />
        </label>
        <div className="markets-filters" role="group" aria-label="Market kind">
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

      {status === 'error' && views.length === 0 ? (
        <div className="markets-empty">
          <h2>Cannot reach the matching engine</h2>
          <p>{error}</p>
          <button type="button" className="landing-button primary" onClick={load}>
            Try again
          </button>
          <Link to="/">Back to the landing page</Link>
        </div>
      ) : status === 'loading' && views.length === 0 ? (
        <div className="market-grid">
          {Array.from({ length: 3 }, (_, index) => (
            <div className="market-card skeleton" key={`skeleton-${index}`} />
          ))}
        </div>
      ) : visible.length === 0 ? (
        <div className="markets-empty">
          <h2>No markets match that filter</h2>
          <p>Try a different name, market ID or event number.</p>
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
        <div className="market-grid">
          {visible.map((view) => (
            <MarketCard key={view.config.id} view={view} />
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
