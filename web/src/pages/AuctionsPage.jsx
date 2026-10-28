import { useCallback, useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  acceptAuction,
  createAuction,
  expireAuction,
  getAuctions,
  getMarkets,
} from '../api'
import { formatPrice, formatQty } from '../format'
import {
  auctionPhase,
  auctionPriceTicks,
  formatCountdown,
} from '../lib/dutchAuction'
import { nextClientOrderId } from '../protocol'
import { useAuthStore } from '../stores/useAuthStore'
import '../shell.css'

const POLL_MS = 2_000

const phaseLabel = {
  scheduled: 'Scheduled',
  active: 'Live',
  ended: 'Ended',
  sold: 'Sold out',
  expired: 'Expired',
}

const collarFor = (market) => {
  const faceTicks = Math.floor(
    market.face_value_minor / market.tick_size_minor,
  )
  return {
    floor: Math.ceil((faceTicks * market.collar_floor_bps) / 10_000),
    cap: Math.floor((faceTicks * market.collar_cap_bps) / 10_000),
  }
}

export function AuctionsPage() {
  const user = useAuthStore((state) => state.user)
  const userId = user?.user_id ?? null
  const [auctions, setAuctions] = useState([])
  const [markets, setMarkets] = useState([])
  const [offset, setOffset] = useState(0)
  const [clock, setClock] = useState(() => Date.now())
  const [status, setStatus] = useState('loading')
  const [error, setError] = useState(null)
  const [notice, setNotice] = useState(null)
  const [busy, setBusy] = useState(false)
  const [qtyById, setQtyById] = useState({})

  const [form, setForm] = useState({
    marketId: '',
    qty: '2',
    startTicks: '',
    floorTicks: '',
    durationSeconds: '60',
  })

  const load = useCallback(async () => {
    try {
      const [auctionPage, marketList] = await Promise.all([
        getAuctions(),
        getMarkets(),
      ])
      setAuctions(auctionPage.items)
      setMarkets(marketList)
      setForm((current) => {
        if (current.marketId || marketList.length === 0) return current
        const market = marketList[0].config
        const collar = collarFor(market)
        return {
          ...current,
          marketId: String(market.id),
          startTicks: String(collar.cap),
          floorTicks: String(collar.floor),
        }
      })
      setOffset(auctionPage.now_ms - Date.now())
      setStatus('ready')
      setError(null)
    } catch (nextError) {
      setError(nextError.message)
      setStatus('error')
    }
  }, [])

  useEffect(() => {
    const initial = window.setTimeout(load, 0)
    const poll = window.setInterval(load, POLL_MS)
    const tick = window.setInterval(() => setClock(Date.now()), 250)
    return () => {
      window.clearTimeout(initial)
      window.clearInterval(poll)
      window.clearInterval(tick)
    }
  }, [load])

  const nowMs = clock + offset

  const marketsById = useMemo(
    () => new Map(markets.map((market) => [market.config.id, market.config])),
    [markets],
  )

  const selectedMarket = form.marketId
    ? marketsById.get(Number(form.marketId))
    : markets[0]?.config

  const pickMarket = (marketId) => {
    const market = marketsById.get(Number(marketId))
    const collar = market ? collarFor(market) : { floor: 0, cap: 0 }
    setForm((current) => ({
      ...current,
      marketId: String(marketId),
      startTicks: String(collar.cap),
      floorTicks: String(collar.floor),
    }))
  }

  const handleCreate = async (event) => {
    event.preventDefault()
    if (userId == null) {
      setError('Log in to start an auction.')
      return
    }
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      const durationMs = Number(form.durationSeconds) * 1000
      const created = await createAuction({
        client_order_id: nextClientOrderId(),
        market_id: Number(form.marketId || selectedMarket?.id),
        qty: Number(form.qty),
        start_price_ticks: Number(form.startTicks),
        floor_price_ticks: Number(form.floorTicks),
        duration_ms: durationMs,
      })
      setNotice(`Auction #${created.id} is live on ${created.market_name}.`)
      await load()
    } catch (nextError) {
      setError(nextError.message)
    } finally {
      setBusy(false)
    }
  }

  const handleAccept = async (auction) => {
    if (userId == null) {
      setError('Log in to accept an auction.')
      return
    }
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      const qty = Number(qtyById[auction.id] ?? 1)
      const price = auctionPriceTicks(auction, nowMs)
      const accepted = await acceptAuction(auction.id, {
        client_order_id: nextClientOrderId(),
        qty,
        max_price_ticks: price,
      })
      setNotice(
        `Bought ${formatQty(accepted.qty)} from auction #${auction.id} at ${formatPrice(
          marketsById.get(auction.market_id),
          accepted.price_ticks,
        )}.`,
      )
      await load()
    } catch (nextError) {
      setError(nextError.message)
    } finally {
      setBusy(false)
    }
  }

  const handleExpire = async (auction) => {
    if (userId == null) {
      setError('Log in to close an auction.')
      return
    }
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      await expireAuction(auction.id)
      setNotice(`Auction #${auction.id} closed and unsold passes were returned.`)
      await load()
    } catch (nextError) {
      setError(nextError.message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="auction-page">
      <div className="auction-heading">
        <div>
          <span className="eyebrow">Liquidation</span>
          <h2>Dutch auctions</h2>
        </div>
        <p className="auction-intro">
          A seller reserves passes and publishes a linear price decay. Anyone can
          take the current slice; the first accept that fits wins. Unsold passes
          return to the seller after the deadline.
        </p>
      </div>

      {error && <p className="auction-error">{error}</p>}
      {notice && <p className="auction-notice">{notice}</p>}
      {!user && (
        <p className="auction-guest-note">
          Browsing as a guest. <Link to="/login" state={{ from: '/auctions' }}>Log in</Link>{' '}
          to start or accept an auction.
        </p>
      )}

      <section className="auction-panel">
        <h3>Open auctions</h3>
        {status === 'loading' && <p className="auction-empty">Loading…</p>}
        {status === 'ready' && auctions.length === 0 && (
          <p className="auction-empty">No auctions yet. Create one below.</p>
        )}
        <div className="auction-grid">
          {auctions.map((auction) => {
            const market = marketsById.get(auction.market_id)
            const phase = auctionPhase(auction, nowMs)
            const price = auctionPriceTicks(auction, nowMs)
            const isSeller = auction.seller_id === userId
            const remainingMs = auction.end_ms - nowMs
            return (
              <article key={auction.id} className="auction-card">
                <header>
                  <span className="auction-id">#{auction.id}</span>
                  <h4>{auction.market_name}</h4>
                  <span className={`auction-phase phase-${phase}`}>
                    {phaseLabel[phase]}
                  </span>
                </header>
                <dl>
                  <div>
                    <dt>Current price</dt>
                    <dd>{formatPrice(market, price)}</dd>
                  </div>
                  <div>
                    <dt>Floor</dt>
                    <dd>{formatPrice(market, auction.floor_price_ticks)}</dd>
                  </div>
                  <div>
                    <dt>Remaining</dt>
                    <dd>
                      {formatQty(auction.qty_remaining)} /{' '}
                      {formatQty(auction.qty_total)}
                    </dd>
                  </div>
                  <div>
                    <dt>Seller</dt>
                    <dd>User {auction.seller_id}</dd>
                  </div>
                  <div>
                    <dt>{phase === 'active' ? 'Ends in' : 'Window'}</dt>
                    <dd>
                      {phase === 'active'
                        ? formatCountdown(remainingMs)
                        : formatCountdown(
                            auction.end_ms - auction.start_ms,
                          )}
                    </dd>
                  </div>
                </dl>
                {phase === 'active' && (
                  user ? (
                    <div className="auction-actions">
                      <label>
                        Qty
                        <input
                          type="number"
                          min="1"
                          max={auction.qty_remaining}
                          value={qtyById[auction.id] ?? 1}
                          onChange={(event) =>
                            setQtyById((current) => ({
                              ...current,
                              [auction.id]: event.target.value,
                            }))
                          }
                        />
                      </label>
                      <button
                        type="button"
                        disabled={busy || auction.seller_id === userId}
                        onClick={() => handleAccept(auction)}
                      >
                        Accept at {formatPrice(market, price)}
                      </button>
                    </div>
                  ) : (
                    <div className="auction-actions">
                      <Link
                        className="auction-login"
                        to="/login"
                        state={{ from: '/auctions' }}
                      >
                        Log in to accept
                      </Link>
                    </div>
                  )
                )}
                {phase === 'ended' && isSeller && (
                  <div className="auction-actions">
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => handleExpire(auction)}
                    >
                      Close and return unsold
                    </button>
                  </div>
                )}
                {auction.seller_id === userId && phase !== 'sold' && (
                  <p className="auction-hint">You are the seller.</p>
                )}
              </article>
            )
          })}
        </div>
      </section>

      <section className="auction-panel">
        <h3>Start an auction</h3>
        {user ? (
          <form className="auction-form" onSubmit={handleCreate}>
          <label>
            Market
            <select
              value={form.marketId || selectedMarket?.id || ''}
              onChange={(event) => pickMarket(event.target.value)}
            >
              {markets.map((market) => (
                <option key={market.config.id} value={market.config.id}>
                  {market.config.name}
                </option>
              ))}
            </select>
          </label>
          <label>
            Quantity
            <input
              type="number"
              min="1"
              value={form.qty}
              onChange={(event) =>
                setForm((current) => ({ ...current, qty: event.target.value }))
              }
            />
          </label>
          <label>
            Start price (ticks)
            <input
              type="number"
              min="1"
              value={form.startTicks}
              onChange={(event) =>
                setForm((current) => ({
                  ...current,
                  startTicks: event.target.value,
                }))
              }
            />
          </label>
          <label>
            Floor price (ticks)
            <input
              type="number"
              min="1"
              value={form.floorTicks}
              onChange={(event) =>
                setForm((current) => ({
                  ...current,
                  floorTicks: event.target.value,
                }))
              }
            />
          </label>
          <label>
            Window (seconds)
            <input
              type="number"
              min="1"
              value={form.durationSeconds}
              onChange={(event) =>
                setForm((current) => ({
                  ...current,
                  durationSeconds: event.target.value,
                }))
              }
            />
          </label>
          <button type="submit" disabled={busy || !selectedMarket}>
            {busy ? 'Working…' : 'Reserve passes and publish'}
          </button>
        </form>
        ) : (
          <div className="auction-guest">
            <p>Log in to reserve passes and publish an auction.</p>
            <Link to="/login" state={{ from: '/auctions' }}>
              Log in
            </Link>
          </div>
        )}
        {selectedMarket && (
          <p className="auction-hint">
            {selectedMarket.name}: collar {formatPrice(selectedMarket, collarFor(selectedMarket).floor)}{' '}
            – {formatPrice(selectedMarket, collarFor(selectedMarket).cap)}. The
            seller must already hold the passes.
          </p>
        )}
      </section>
    </div>
  )
}
