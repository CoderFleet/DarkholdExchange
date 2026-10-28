import { useCallback, useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import { getMarkets, getStateHash } from '../api'
import {
  ArrowRight,
  DataObjectIcon,
  MemoryIcon,
  ShieldCheckIcon,
} from '../components/Icon'
import { MarketCard } from '../components/MarketCard'
import { formatMoney, formatPercent, formatPrice, formatQty } from '../format'
import { sortMarketViews, spreadTicks } from '../lib/marketOverview'
import { useAuthStore } from '../stores/useAuthStore'
import './marketing.css'

const POLL_MS = 5_000

const PROOFS = [
  { label: 'i64 minor units', Icon: DataObjectIcon, tone: 'purple' },
  { label: 'Single-writer core', Icon: MemoryIcon, tone: 'green' },
  { label: 'Crash-safe WAL', Icon: ShieldCheckIcon, tone: 'purple' },
]

const TRUST_ITEMS = [
  {
    number: '01',
    title: 'Deterministic core',
    body: 'FIFO price-time priority with sequenced execution deltas.',
    tag: 'Zero heuristics',
  },
  {
    number: '02',
    title: 'Proof of solvency',
    body: 'Merkle-sum liabilities with an in-browser client verifier.',
    tag: 'Public root',
  },
  {
    number: '03',
    title: 'Opt-in escrow',
    body: 'Cryptographically signed delivery receipts and atomic handoff.',
    tag: 'Signed receipts',
  },
  {
    number: '04',
    title: 'Dutch liquidation',
    body: 'Published linear price decay for secondary discovery.',
    tag: 'Collar bounded',
  },
]

const PIPELINE = [
  {
    number: '01',
    title: 'Fund your wallet',
    body: 'Top up cash or receive passes directly into a segregated ledger account.',
  },
  {
    number: '02',
    title: 'Choose a market',
    body: 'Every book publishes its face value, collar, taker fee and holding limit up front.',
  },
  {
    number: '03',
    title: 'Trade the book',
    body: 'Limit, market and cancel orders match in one deterministic single-writer core.',
  },
  {
    number: '04',
    title: 'Verify your balance',
    body: 'Check your leaf against the published Merkle-sum root in your browser.',
  },
]

function shortHash(hash) {
  if (!hash) return null
  return `${hash.slice(0, 8)}…${hash.slice(-6)}`
}

function LandingPulse({ view, loading, error }) {
  const market = view?.config
  const snapshot = view?.snapshot
  const asks = [...(snapshot?.asks ?? [])].slice(0, 3).reverse()
  const bids = (snapshot?.bids ?? []).slice(0, 3)
  const maxQty = Math.max(
    1,
    ...asks.map((level) => Number(level.qty)),
    ...bids.map((level) => Number(level.qty)),
  )
  const bestBid = bids[0]?.price_ticks
  const bestAsk = asks[asks.length - 1]?.price_ticks
  const spread = spreadTicks(snapshot)
  const last = snapshot?.last_trade_price
  const mid =
    bestBid != null && bestAsk != null ? Math.round((bestBid + bestAsk) / 2) : null

  return (
    <aside className="landing-pulse" aria-label="Live market pulse">
      <div className="pulse-head">
        <span className={`pulse-dot ${loading ? 'checking' : error ? 'offline' : 'live'}`} />
        <span className="pulse-market">
          {market ? `Market ${String(market.id).padStart(2, '0')}` : 'Market pulse'}
        </span>
        <span className="pulse-kind">
          {market ? (market.escrow_enabled ? 'Escrow · LOB' : 'Spot · LOB') : 'Connecting'}
        </span>
      </div>

      {market ? (
        <>
          <div className="pulse-title-row">
            <h2>{market.name}</h2>
            <span className="pulse-seq">Book seq #{snapshot?.book_seq ?? '—'}</span>
          </div>
          <div className="pulse-price-row">
            <div>
              <span className="pulse-price-label">Last print</span>
              <strong className="pulse-price">
                {last != null ? formatPrice(market, last) : mid != null ? formatPrice(market, mid) : '—'}
              </strong>
            </div>
            <div className="pulse-meta">
              <span>{formatMoney(market.face_value_minor)} face</span>
              <span>{formatPercent(market.taker_fee_bps)} taker fee</span>
            </div>
          </div>

          <div className="pulse-book">
            <div className="pulse-book-head">
              <span>Price</span>
              <span>Qty</span>
            </div>
            {asks.map((level) => (
              <div className="pulse-row ask" key={`ask-${level.price_ticks}`}>
                <span
                  className="pulse-depth"
                  style={{ width: `${Math.min(100, (Number(level.qty) / maxQty) * 100)}%` }}
                />
                <span>{formatPrice(market, level.price_ticks)}</span>
                <span>{formatQty(level.qty)}</span>
              </div>
            ))}
            {asks.length === 0 && <div className="pulse-empty">No asks resting</div>}
            <div className="pulse-spread">
              <span>Spread</span>
              <strong>
                {spread != null
                  ? `${formatQty(spread)} ticks · ${formatPrice(market, spread)}`
                  : '—'}
              </strong>
              <span>{mid != null ? `Mid ${formatPrice(market, mid)}` : ''}</span>
            </div>
            {bids.map((level) => (
              <div className="pulse-row bid" key={`bid-${level.price_ticks}`}>
                <span
                  className="pulse-depth"
                  style={{ width: `${Math.min(100, (Number(level.qty) / maxQty) * 100)}%` }}
                />
                <span>{formatPrice(market, level.price_ticks)}</span>
                <span>{formatQty(level.qty)}</span>
              </div>
            ))}
            {bids.length === 0 && <div className="pulse-empty">No bids resting</div>}
          </div>

          <div className="pulse-foot">
            <span>
              Bid <b>{bestBid != null ? formatPrice(market, bestBid) : '—'}</b>
            </span>
            <span>
              Ask <b>{bestAsk != null ? formatPrice(market, bestAsk) : '—'}</b>
            </span>
            <Link to={`/markets/${market.id}`}>
              View market <ArrowRight size={12} />
            </Link>
          </div>
        </>
      ) : (
        <div className="pulse-placeholder">
          <strong>{error ? 'Matching engine offline' : 'Connecting to the matching engine…'}</strong>
          <p>
            {error
              ? 'Start the gateway to stream live books, or browse the market index while it reconnects.'
              : 'The terminal is loading the latest book snapshot.'}
          </p>
          <Link to="/markets">
            Browse markets <ArrowRight size={12} />
          </Link>
        </div>
      )}
    </aside>
  )
}

export function LandingPage() {
  const user = useAuthStore((state) => state.user)
  const [views, setViews] = useState([])
  const [stateHash, setStateHash] = useState(null)
  const [status, setStatus] = useState('loading')
  const [error, setError] = useState(null)

  const load = useCallback(async () => {
    const [marketsResult, hashResult] = await Promise.allSettled([
      getMarkets(),
      getStateHash(),
    ])
    if (marketsResult.status === 'fulfilled') {
      setViews(marketsResult.value)
      setStatus('ready')
      setError(null)
    } else {
      setStatus('error')
      setError(marketsResult.reason?.message ?? 'Cannot reach the gateway.')
    }
    if (hashResult.status === 'fulfilled') {
      setStateHash(hashResult.value.state_hash)
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

  const featured = useMemo(() => sortMarketViews(views).slice(0, 3), [views])
  const pulse = useMemo(
    () => featured.find((view) => view.snapshot?.last_trade_price != null) ?? featured[0] ?? null,
    [featured],
  )

  return (
    <main className="landing">
      <section className="landing-hero">
        <div className="landing-copy">
          <span className="eyebrow">Webster 2026 · Secondary market infrastructure</span>
          <h1>Event passes, traded like real markets.</h1>
          <p className="landing-lede">
            A deterministic matching engine for event access. Server-enforced
            price collars, integer ticks, and a solvency proof you can verify in
            your browser.
          </p>
          <div className="landing-actions">
            <Link className="landing-button primary" to="/markets">
              Explore markets <ArrowRight size={16} />
            </Link>
            <a className="landing-button ghost" href="#how-it-works">
              How it works
            </a>
          </div>
          <ul className="landing-proofs">
            {PROOFS.map(({ label, Icon, tone }) => (
              <li key={label}>
                <Icon size={15} className={`proof-icon ${tone}`} />
                <span>{label}</span>
              </li>
            ))}
          </ul>
        </div>

        <LandingPulse
          view={pulse}
          loading={status === 'loading'}
          error={status === 'error' ? error : null}
        />
      </section>

      <section className="trust-band" aria-label="System guarantees">
        <div className="trust-strip">
          {TRUST_ITEMS.map((item) => (
            <article className="trust-card" key={item.number}>
              <div>
                <span className="trust-number">{item.number}</span>
                <h3>{item.title}</h3>
                <p>{item.body}</p>
              </div>
              <span className="trust-tag">{item.tag}</span>
            </article>
          ))}
        </div>
      </section>

      <section className="landing-section">
        <header className="section-head">
          <div>
            <span className="eyebrow">Execution books</span>
            <h2>Featured markets</h2>
          </div>
          <Link className="section-link" to="/markets">
            View all markets <ArrowRight size={12} />
          </Link>
        </header>

        {status === 'error' && views.length === 0 ? (
          <div className="landing-empty">
            <p>Markets are unavailable until the gateway is running.</p>
            <span>{error}</span>
          </div>
        ) : (
          <div className="market-grid">
            {featured.length === 0
              ? Array.from({ length: 3 }, (_, index) => (
                  <div className="market-card skeleton" key={`skeleton-${index}`} />
                ))
              : featured.map((view) => <MarketCard key={view.config.id} view={view} />)}
          </div>
        )}
      </section>

      <section className="landing-section pipeline" id="how-it-works">
        <span className="eyebrow">System architecture</span>
        <h2>Deterministic pipeline</h2>
        <p className="pipeline-lede">
          Every transition advances through an invariant state machine with zero
          floating-point ambiguity.
        </p>
        <ol className="pipeline-steps">
          {PIPELINE.map((step) => (
            <li key={step.number}>
              <span className="pipeline-number">{step.number}</span>
              <strong>{step.title}</strong>
              <p>{step.body}</p>
            </li>
          ))}
        </ol>
      </section>

      <section className="landing-cta">
        <span className="eyebrow">Order engine ready</span>
        <h2>The book is open.</h2>
        <p>
          Create an account, fund a wallet, and trade event passes on a matching
          engine built for deterministic settlement.
        </p>
        <div className="landing-actions centered">
          <Link className="landing-button primary" to={user ? '/markets' : '/signup'}>
            {user ? 'Enter the terminal' : 'Open an account'}
          </Link>
          <Link className="landing-button ghost" to="/markets">
            Explore markets
          </Link>
        </div>
        <div className="cta-meta">
          <span className="cta-status">
            <span className="dot" />
            Deterministic core
          </span>
          <span>i64 minor units</span>
          <span>Crash-safe WAL</span>
          {stateHash && <span>State hash {shortHash(stateHash)}</span>}
        </div>
      </section>
    </main>
  )
}
