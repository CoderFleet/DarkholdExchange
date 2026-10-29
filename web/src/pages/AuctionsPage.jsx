import { useCallback, useEffect, useMemo, useState } from 'react'
import { Link } from 'react-router-dom'
import {
  acceptAuction,
  createAuction,
  expireAuction,
  getAuctions,
  getMarkets,
  getBatchAuctions,
  createBatchAuction,
  commitBatchBid,
  revealBatchBid,
  commitBatchAsk,
  revealBatchAsk,
  settleBatchAuction,
} from '../api'
import { formatPrice, formatQty } from '../format'
import {
  auctionPhase,
  auctionPriceTicks,
  formatCountdown,
} from '../lib/dutchAuction'
import { computeCommitment, generateSalt } from '../lib/batchAuction'
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
  const isAdmin = user?.role === 'admin' || user?.role === 'issuer'

  const [tab, setTab] = useState('dutch') // 'dutch' | 'batch'
  const [auctions, setAuctions] = useState([])
  const [batchAuctions, setBatchAuctions] = useState([])
  const [markets, setMarkets] = useState([])
  const [offset, setOffset] = useState(0)
  const [clock, setClock] = useState(() => Date.now())
  const [status, setStatus] = useState('loading')
  const [error, setError] = useState(null)
  const [notice, setNotice] = useState(null)
  const [busy, setBusy] = useState(false)
  const [qtyById, setQtyById] = useState({})

  // Form for Dutch
  const [form, setForm] = useState({
    marketId: '',
    qty: '2',
    startTicks: '',
    floorTicks: '',
    durationSeconds: '60',
  })

  // Form for Batch (admin/issuer only: schedule a pooled clearing window)
  const [batchForm, setBatchForm] = useState({
    marketId: '',
    floorTicks: '',
    commitSeconds: '30',
    revealSeconds: '30',
  })

  // Bid state for Batch: { [auctionId]: { priceTicks, qty, commitmentHex, saltHex } }
  const [batchBidsState, setBatchBidsState] = useState(() => {
    try {
      const stored = localStorage.getItem('dx_batch_bids')
      return stored ? JSON.parse(stored) : {}
    } catch {
      return {}
    }
  })

  const saveBatchBidState = (auctionId, data) => {
    setBatchBidsState((prev) => {
      const next = { ...prev, [auctionId]: { ...prev[auctionId], ...data } }
      try {
        localStorage.setItem('dx_batch_bids', JSON.stringify(next))
      } catch (err) {
        console.warn('Failed to cache batch bid', err)
      }
      return next
    })
  }

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
      setBatchForm((current) => {
        if (current.marketId || marketList.length === 0) return current
        const market = marketList[0].config
        const collar = collarFor(market)
        return {
          ...current,
          marketId: String(market.id),
          floorTicks: String(collar.floor),
        }
      })
      setOffset(auctionPage.now_ms - Date.now())
      setStatus('ready')
      setError(null)

      // Batch auctions: best-effort — don't crash the page if endpoint is missing
      try {
        const batchPage = await getBatchAuctions()
        setBatchAuctions(batchPage.items)
      } catch (batchErr) {
        console.warn('Batch auctions endpoint unavailable:', batchErr)
        setBatchAuctions([])
      }
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
    () => new Map(markets.map((m) => [m.config.id, m.config])),
    [markets],
  )
  const selectedMarket = form.marketId
    ? marketsById.get(Number(form.marketId))
    : markets[0]?.config
  const selectedBatchMarket = batchForm.marketId
    ? marketsById.get(Number(batchForm.marketId))
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

  const pickBatchMarket = (marketId) => {
    const market = marketsById.get(Number(marketId))
    const collar = market ? collarFor(market) : { floor: 0, cap: 0 }
    setBatchForm((current) => ({
      ...current,
      marketId: String(marketId),
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
      setNotice(`Dutch auction #${created.id} is live on ${created.market_name}.`)
      await load()
    } catch (nextError) {
      setError(nextError.message)
    } finally {
      setBusy(false)
    }
  }

  const handleCreateBatch = async (event) => {
    event.preventDefault()
    if (userId == null) {
      setError('Log in to schedule a clearing window.')
      return
    }
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      const commitDurationMs = Number(batchForm.commitSeconds) * 1000
      const revealDurationMs = Number(batchForm.revealSeconds) * 1000
      const created = await createBatchAuction({
        client_order_id: nextClientOrderId(),
        market_id: Number(batchForm.marketId || selectedBatchMarket?.id),
        floor_price_ticks: Number(batchForm.floorTicks),
        commit_duration_ms: commitDurationMs,
        reveal_duration_ms: revealDurationMs,
      })
      setNotice(
        `Clearing window #${created.id} scheduled. Sellers and buyers can now commit sealed orders into the pool.`,
      )
      await load()
    } catch (nextError) {
      setError(nextError.message)
    } finally {
      setBusy(false)
    }
  }

  const handleCommitBid = async (auction) => {
    if (userId == null) {
      setError('Log in to commit a sealed order.')
      return
    }
    const currentBid = batchBidsState[auction.id]
    const side = currentBid?.side === 'sell' ? 'sell' : 'buy'
    const priceTicks = Number(currentBid?.priceTicks || auction.floor_price_ticks)
    const qty = Number(currentBid?.qty || 1)

    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      const salt = generateSalt()
      const { commitmentHex, saltHex } = await computeCommitment(priceTicks, qty, salt)
      const clientOrderId = nextClientOrderId()

      if (side === 'sell') {
        await commitBatchAsk(auction.id, {
          client_order_id: clientOrderId,
          commitment: commitmentHex,
        })
      } else {
        await commitBatchBid(auction.id, {
          client_order_id: clientOrderId,
          commitment: commitmentHex,
        })
      }

      saveBatchBidState(auction.id, {
        clientOrderId,
        priceTicks,
        qty,
        side,
        commitmentHex,
        saltHex,
        committed: true,
      })
      setNotice(
        side === 'sell'
          ? `Sealed ask committed for Batch Auction #${auction.id}! Your passes reserve at reveal.`
          : `Sealed bid committed for Batch Auction #${auction.id}! Keep secret until reveal phase.`,
      )
      await load()
    } catch (nextError) {
      setError(nextError.message)
    } finally {
      setBusy(false)
    }
  }

  const handleRevealBid = async (auction) => {
    if (userId == null) {
      setError('Log in to reveal your sealed order.')
      return
    }
    const currentBid = batchBidsState[auction.id]
    if (!currentBid?.committed || !currentBid?.saltHex) {
      setError('No committed sealed order found for this auction.')
      return
    }

    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      const reveal = {
        client_order_id: currentBid.clientOrderId,
        price_ticks: currentBid.priceTicks,
        qty: currentBid.qty,
        salt: currentBid.saltHex,
      }
      if (currentBid.side === 'sell') {
        await revealBatchAsk(auction.id, reveal)
      } else {
        await revealBatchBid(auction.id, reveal)
      }

      saveBatchBidState(auction.id, { revealed: true })
      setNotice(
        currentBid.side === 'sell'
          ? `Ask revealed for Batch Auction #${auction.id}! Passes are reserved until clearing.`
          : `Bid revealed for Batch Auction #${auction.id}! Cash collateral reserved.`,
      )
      await load()
    } catch (nextError) {
      setError(nextError.message)
    } finally {
      setBusy(false)
    }
  }

  const handleSettleBatch = async (auction) => {
    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      const res = await settleBatchAuction(auction.id, {
        client_order_id: nextClientOrderId(),
      })
      setNotice(
        `Auction #${auction.id} settled! Fills: ${res.fills.length}. Uniform price applied.`,
      )
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
      setError('Log in to expire an auction.')
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
          <span className="eyebrow">Price Discovery & Fair Clearing</span>
          <h2>Auctions</h2>
        </div>
        <p className="auction-intro">
          {tab === 'dutch'
            ? 'Continuous Dutch auctions start high and decay over time until a buyer accepts or the floor is reached.'
            : 'Frequent Batch Auctions collect sealed bids and clear all winning orders simultaneously at one uniform price.'}
        </p>
      </div>

      <div className="auction-tabs-bar">
        <div className="auction-tabs" role="tablist" aria-label="Auction Types">
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'dutch'}
            className={`auction-tab-btn ${tab === 'dutch' ? 'active' : ''}`}
            onClick={() => setTab('dutch')}
          >
            <span>Dutch Auctions</span>
            <span className="auction-tab-count">{auctions.length}</span>
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === 'batch'}
            className={`auction-tab-btn ${tab === 'batch' ? 'active' : ''}`}
            onClick={() => setTab('batch')}
          >
            <span>Batch Auctions</span>
            <span className="auction-tab-count">{batchAuctions.length}</span>
          </button>
        </div>
      </div>

      {error && <p className="auction-error">{error}</p>}
      {notice && <p className="auction-notice">{notice}</p>}
      {!user && (
        <p className="auction-guest-note">
          Browsing as a guest.{' '}
          <Link to="/login" state={{ from: '/auctions' }}>
            Log in
          </Link>{' '}
          to participate or publish an auction.
        </p>
      )}

      {/* ── DUTCH AUCTION VIEW ────────────────────────────────────────────── */}
      {tab === 'dutch' && (
        <>
          <section className="auction-panel">
            <h3>Open Dutch Auctions</h3>
            {status === 'loading' && <p className="auction-empty">Loading…</p>}
            {status === 'ready' && auctions.length === 0 && (
              <p className="auction-empty">No Dutch auctions yet. Create one below.</p>
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
                        <dd>{formatQty(auction.qty_remaining)}</dd>
                      </div>
                      <div>
                        <dt>Time</dt>
                        <dd>{formatCountdown(remainingMs)}</dd>
                      </div>
                    </dl>
                    {phase === 'active' && !isSeller && (
                      <div className="auction-actions">
                        <input
                          type="number"
                          min="1"
                          max={auction.qty_remaining}
                          value={qtyById[auction.id] ?? 1}
                          onChange={(e) =>
                            setQtyById((prev) => ({
                              ...prev,
                              [auction.id]: e.target.value,
                            }))
                          }
                        />
                        <button
                          type="button"
                          disabled={busy || !userId}
                          onClick={() => handleAccept(auction)}
                        >
                          Buy at current price
                        </button>
                      </div>
                    )}
                    {phase === 'ended' && isSeller && (
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => handleExpire(auction)}
                      >
                        Reclaim unsold passes
                      </button>
                    )}
                  </article>
                )
              })}
            </div>
          </section>

          <section className="auction-panel">
            <h3>Create a Dutch Auction</h3>
            {user ? (
              <form className="auction-form" onSubmit={handleCreate}>
                <label>
                  Market
                  <select
                    value={form.marketId || selectedMarket?.id || ''}
                    onChange={(e) => pickMarket(e.target.value)}
                  >
                    {markets.map((m) => (
                      <option key={m.config.id} value={m.config.id}>
                        {m.config.name}
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
                    onChange={(e) => setForm((c) => ({ ...c, qty: e.target.value }))}
                  />
                </label>
                <label>
                  Start price (ticks)
                  <input
                    type="number"
                    value={form.startTicks}
                    onChange={(e) => setForm((c) => ({ ...c, startTicks: e.target.value }))}
                  />
                </label>
                <label>
                  Floor price (ticks)
                  <input
                    type="number"
                    value={form.floorTicks}
                    onChange={(e) => setForm((c) => ({ ...c, floorTicks: e.target.value }))}
                  />
                </label>
                <label>
                  Window (seconds)
                  <input
                    type="number"
                    min="1"
                    value={form.durationSeconds}
                    onChange={(e) => setForm((c) => ({ ...c, durationSeconds: e.target.value }))}
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
          </section>
        </>
      )}

      {/* ── BATCH AUCTION VIEW (COMMIT-REVEAL) ─────────────────────────────── */}
      {tab === 'batch' && (
        <>
          <section className="auction-panel">
            <div className="panel-header">
              <h3>Active Clearing Windows</h3>
              <span className="panel-meta">{batchAuctions.length} active</span>
            </div>
            {status === 'loading' && <p className="auction-empty">Loading…</p>}
            {status === 'ready' && batchAuctions.length === 0 && (
              <p className="auction-empty">
                {isAdmin
                  ? 'No active batch auction windows. Schedule one below.'
                  : 'No batch auction windows are active right now. Check back when the exchange opens a clearing round.'}
              </p>
            )}
            <div className="auction-grid">
              {batchAuctions.map((auction) => {
                const market = marketsById.get(auction.market_id)
                const savedBid = batchBidsState[auction.id] || {}
                const savedSide = savedBid.side === 'sell' ? 'sell' : 'buy'

                // Phase time calculation
                const commitRemainingMs = Math.max(0, auction.commit_end_ms - nowMs)
                const revealRemainingMs = Math.max(0, auction.reveal_end_ms - nowMs)

                return (
                  <article key={auction.id} className="auction-card">
                    <header>
                      <span className="auction-id">Batch #{auction.id}</span>
                      <h4>{auction.market_name}</h4>
                      <span className={`auction-phase phase-${auction.phase}`}>
                        {auction.phase.toUpperCase()}
                      </span>
                    </header>
                    <dl>
                      {auction.qty_total != null && (
                        <div>
                          <dt>Pooled supply</dt>
                          <dd>{formatQty(auction.qty_total)} passes</dd>
                        </div>
                      )}
                      <div>
                        <dt>Floor price</dt>
                        <dd>{formatPrice(market, auction.floor_price_ticks)}</dd>
                      </div>
                      <div>
                        <dt>Commit window</dt>
                        <dd>{commitRemainingMs > 0 ? formatCountdown(commitRemainingMs) : 'Closed'}</dd>
                      </div>
                      <div>
                        <dt>Reveal window</dt>
                        <dd>{revealRemainingMs > 0 ? formatCountdown(revealRemainingMs) : 'Closed'}</dd>
                      </div>
                      {auction.clearing_price_ticks && (
                        <div>
                          <dt>Uniform Price</dt>
                          <dd className="auction-card-clearing">
                            {formatPrice(market, auction.clearing_price_ticks)}
                          </dd>
                        </div>
                      )}
                    </dl>

                    {/* Commit Phase Action — everyone can bid, everyone can ask */}
                    {auction.phase === 'commit' && (
                      <div className="batch-bid-actions">
                        <div className="batch-side-toggle" role="radiogroup" aria-label="Order side">
                          <button
                            type="button"
                            className={savedSide === 'buy' ? 'active' : ''}
                            disabled={busy || savedBid.committed}
                            onClick={() => saveBatchBidState(auction.id, { side: 'buy' })}
                          >
                            Buy (bid)
                          </button>
                          <button
                            type="button"
                            className={savedSide === 'sell' ? 'active' : ''}
                            disabled={busy || savedBid.committed}
                            onClick={() => saveBatchBidState(auction.id, { side: 'sell' })}
                          >
                            Sell (ask)
                          </button>
                        </div>
                        <div className="batch-bid-inputs">
                          <label>
                            {savedSide === 'sell' ? 'Min price (ticks)' : 'Price (ticks)'}
                            <input
                              type="number"
                              min="1"
                              placeholder="Ticks"
                              value={savedBid.priceTicks ?? auction.floor_price_ticks}
                              onChange={(e) =>
                                saveBatchBidState(auction.id, { priceTicks: Number(e.target.value) })
                              }
                            />
                          </label>
                          <label>
                            {savedSide === 'sell' ? 'Passes to sell' : 'Quantity'}
                            <input
                              type="number"
                              placeholder="Qty"
                              min="1"
                              value={savedBid.qty ?? 1}
                              onChange={(e) =>
                                saveBatchBidState(auction.id, { qty: Number(e.target.value) })
                              }
                            />
                          </label>
                        </div>
                        {userId ? (
                          <button
                            type="button"
                            disabled={busy || savedBid.committed}
                            onClick={() => handleCommitBid(auction)}
                          >
                            {savedBid.committed
                              ? savedSide === 'sell'
                                ? 'Sealed Ask Committed'
                                : 'Sealed Bid Committed'
                              : savedSide === 'sell'
                                ? 'Commit Sealed Ask (SHA-256)'
                                : 'Commit Sealed Bid (SHA-256)'}
                          </button>
                        ) : (
                          <div className="auction-guest">
                            <p>Log in to commit a sealed order into this window.</p>
                            <Link to="/login" state={{ from: '/auctions' }}>Log in</Link>
                          </div>
                        )}
                        <p className="auction-hint">
                          One sealed order per side per account. Buyers reserve cash and sellers
                          reserve passes when they reveal — nothing locks up during commit.
                        </p>
                      </div>
                    )}

                    {/* Reveal Phase Action */}
                    {auction.phase === 'reveal' && (
                      <div className="auction-actions">
                        <button
                          type="button"
                          disabled={busy || !userId || savedBid.revealed || !savedBid.committed}
                          onClick={() => handleRevealBid(auction)}
                        >
                          {savedBid.revealed
                            ? savedSide === 'sell'
                              ? 'Ask Revealed'
                              : 'Bid Revealed'
                            : savedBid.committed
                              ? 'Reveal Pre-Image'
                              : 'No Commit Made'}
                        </button>
                      </div>
                    )}

                    {/* Settle Action — admin only */}
                    {isAdmin && (auction.phase === 'reveal' || auction.phase === 'commit') && revealRemainingMs === 0 && (
                      <div className="auction-actions">
                        <button
                          type="button"
                          disabled={busy}
                          onClick={() => handleSettleBatch(auction)}
                        >
                          Clear Auction (Uniform Price)
                        </button>
                      </div>
                    )}

                    {auction.phase === 'settled' && (
                      <div className="auction-settled-badge">
                        <span>✓</span> Settled
                        {auction.qty_remaining != null && (
                          <> • Unsold: {formatQty(auction.qty_remaining)}</>
                        )}
                      </div>
                    )}
                  </article>
                )
              })}
            </div>
          </section>

          {userId ? (
            <section className="auction-panel">
              <div className="panel-header">
                <h3>Schedule Clearing Window</h3>
                <span className="panel-meta">{isAdmin ? 'Admin or Issuer only' : 'Unavailable for your role'}</span>
              </div>
              {isAdmin ? (
                <form className="auction-form" onSubmit={handleCreateBatch}>
                <label>
                  Market
                  <select
                    value={batchForm.marketId || selectedBatchMarket?.id || ''}
                    onChange={(e) => pickBatchMarket(e.target.value)}
                  >
                    {markets.map((m) => (
                      <option key={m.config.id} value={m.config.id}>
                        {m.config.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  Floor price (ticks)
                  <input
                    type="number"
                    value={batchForm.floorTicks}
                    onChange={(e) => setBatchForm((c) => ({ ...c, floorTicks: e.target.value }))}
                  />
                </label>
                <label>
                  Commit duration (seconds)
                  <input
                    type="number"
                    min="5"
                    value={batchForm.commitSeconds}
                    onChange={(e) => setBatchForm((c) => ({ ...c, commitSeconds: e.target.value }))}
                  />
                </label>
                <label>
                  Reveal duration (seconds)
                  <input
                    type="number"
                    min="5"
                    value={batchForm.revealSeconds}
                    onChange={(e) => setBatchForm((c) => ({ ...c, revealSeconds: e.target.value }))}
                  />
                </label>
                <button type="submit" disabled={busy || !selectedBatchMarket}>
                  {busy ? 'Working…' : 'Schedule Window'}
                </button>
                </form>
              ) : (
                <p className="auction-hint">
                  Clearing windows are scheduled by the exchange. When one opens for a market you
                  trade, submit a sealed bid (to buy) or a sealed ask (to sell) from the window card
                  above — no inventory is needed to schedule, and no one picks their own timing.
                </p>
              )}
              {isAdmin && selectedBatchMarket && (
                <p className="auction-hint">
                  {selectedBatchMarket.name}: collar {formatPrice(selectedBatchMarket, collarFor(selectedBatchMarket).floor)}{' '}
                  – {formatPrice(selectedBatchMarket, collarFor(selectedBatchMarket).cap)}. The window holds no supply of
                  its own — sellers enter sealed asks and their passes reserve at reveal; nothing is minted.
                </p>
              )}
            </section>
          ) : (
            <section className="auction-panel batch-info-panel">
              <div className="panel-header">
                <h3>How Batch Auctions Work</h3>
                <span className="panel-meta">Pooled Window · Commit-Reveal · Uniform Price</span>
              </div>
              <p className="auction-hint">
                Log in to take part in a pooled clearing window. The exchange schedules one window
                per market; every seller commits a sealed SHA-256 ask and every buyer a sealed bid
                during the commit phase, then both sides reveal simultaneously. The engine clears
                everything at the one uniform price that maximises traded volume — no front-running,
                and no seller can pick their own timing.
              </p>
              <div className="batch-phases">
                <span className="batch-phase-chip">1 · Commit</span>
                <span className="batch-phase-arrow">→</span>
                <span className="batch-phase-chip">2 · Reveal</span>
                <span className="batch-phase-arrow">→</span>
                <span className="batch-phase-chip">3 · Uniform Clear</span>
              </div>
            </section>
          )}
        </>
      )}
    </div>
  )
}
