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

  // Form for Batch
  const [batchForm, setBatchForm] = useState({
    marketId: '',
    qty: '10',
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
      const [auctionPage, batchPage, marketList] = await Promise.all([
        getAuctions(),
        getBatchAuctions(),
        getMarkets(),
      ])
      setAuctions(auctionPage.items)
      setBatchAuctions(batchPage.items)
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
      setError('Log in to start a batch auction.')
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
        qty: Number(batchForm.qty),
        floor_price_ticks: Number(batchForm.floorTicks),
        commit_duration_ms: commitDurationMs,
        reveal_duration_ms: revealDurationMs,
      })
      setNotice(`Batch auction #${created.id} is created in Commit phase.`)
      await load()
    } catch (nextError) {
      setError(nextError.message)
    } finally {
      setBusy(false)
    }
  }

  const handleCommitBid = async (auction) => {
    if (userId == null) {
      setError('Log in to commit a bid.')
      return
    }
    const currentBid = batchBidsState[auction.id]
    const priceTicks = Number(currentBid?.priceTicks || auction.floor_price_ticks)
    const qty = Number(currentBid?.qty || 1)

    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      const salt = generateSalt()
      const { commitmentHex, saltHex } = await computeCommitment(priceTicks, qty, salt)
      const clientOrderId = nextClientOrderId()

      await commitBatchBid(auction.id, {
        client_order_id: clientOrderId,
        commitment: commitmentHex,
      })

      saveBatchBidState(auction.id, {
        clientOrderId,
        priceTicks,
        qty,
        commitmentHex,
        saltHex,
        committed: true,
      })
      setNotice(`Sealed bid committed for Batch Auction #${auction.id}! Keep secret until reveal phase.`)
      await load()
    } catch (nextError) {
      setError(nextError.message)
    } finally {
      setBusy(false)
    }
  }

  const handleRevealBid = async (auction) => {
    if (userId == null) {
      setError('Log in to reveal a bid.')
      return
    }
    const currentBid = batchBidsState[auction.id]
    if (!currentBid?.committed || !currentBid?.saltHex) {
      setError('No committed bid found for this auction.')
      return
    }

    setBusy(true)
    setError(null)
    setNotice(null)
    try {
      await revealBatchBid(auction.id, {
        client_order_id: currentBid.clientOrderId,
        price_ticks: currentBid.priceTicks,
        qty: currentBid.qty,
        salt: currentBid.saltHex,
      })

      saveBatchBidState(auction.id, { revealed: true })
      setNotice(`Bid revealed for Batch Auction #${auction.id}! Cash collateral reserved.`)
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
          <span className="eyebrow">Fair Liquidation</span>
          <h2>Auctions</h2>
        </div>
        <p className="auction-intro">
          Continuous Dutch auctions decay linearly in real-time. Frequent Batch Auctions use sealed
          cryptographic commit-reveal to eliminate front-running and clear at one uniform price.
        </p>

        {/* Tab switcher */}
        <div style={{ display: 'flex', gap: '8px', marginTop: '16px' }}>
          <button
            type="button"
            className={tab === 'dutch' ? 'button-primary' : 'button-secondary'}
            onClick={() => setTab('dutch')}
          >
            Dutch Auctions ({auctions.length})
          </button>
          <button
            type="button"
            className={tab === 'batch' ? 'button-primary' : 'button-secondary'}
            onClick={() => setTab('batch')}
          >
            Batch Auctions (Commit-Reveal) ({batchAuctions.length})
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
                    value={form.marketId}
                    onChange={(e) => setForm((c) => ({ ...c, marketId: e.target.value }))}
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
            <h3>Batch Auctions (Commit-Reveal Uniform Clearing)</h3>
            {status === 'loading' && <p className="auction-empty">Loading…</p>}
            {status === 'ready' && batchAuctions.length === 0 && (
              <p className="auction-empty">No batch auctions active. Create one below.</p>
            )}
            <div className="auction-grid">
              {batchAuctions.map((auction) => {
                const market = marketsById.get(auction.market_id)
                const isSeller = auction.seller_id === userId
                const savedBid = batchBidsState[auction.id] || {}

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
                      <div>
                        <dt>Total supply</dt>
                        <dd>{formatQty(auction.qty_total)} passes</dd>
                      </div>
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
                          <dd style={{ color: '#22c55e', fontWeight: 'bold' }}>
                            {formatPrice(market, auction.clearing_price_ticks)}
                          </dd>
                        </div>
                      )}
                    </dl>

                    {/* Commit Phase Action */}
                    {auction.phase === 'commit' && !isSeller && (
                      <div className="auction-actions" style={{ flexDirection: 'column', gap: '8px' }}>
                        <div style={{ display: 'flex', gap: '8px' }}>
                          <input
                            type="number"
                            placeholder="Price ticks"
                            value={savedBid.priceTicks ?? auction.floor_price_ticks}
                            onChange={(e) =>
                              saveBatchBidState(auction.id, { priceTicks: Number(e.target.value) })
                            }
                          />
                          <input
                            type="number"
                            placeholder="Qty"
                            min="1"
                            value={savedBid.qty ?? 1}
                            onChange={(e) =>
                              saveBatchBidState(auction.id, { qty: Number(e.target.value) })
                            }
                          />
                        </div>
                        <button
                          type="button"
                          disabled={busy || !userId || savedBid.committed}
                          onClick={() => handleCommitBid(auction)}
                        >
                          {savedBid.committed ? 'Sealed Bid Committed ✅' : 'Commit Sealed Bid (SHA-256)'}
                        </button>
                      </div>
                    )}

                    {/* Reveal Phase Action */}
                    {auction.phase === 'reveal' && !isSeller && (
                      <div className="auction-actions">
                        <button
                          type="button"
                          disabled={busy || !userId || savedBid.revealed || !savedBid.committed}
                          onClick={() => handleRevealBid(auction)}
                        >
                          {savedBid.revealed
                            ? 'Bid Revealed ✅'
                            : savedBid.committed
                              ? 'Reveal Pre-Image'
                              : 'No Commit Made'}
                        </button>
                      </div>
                    )}

                    {/* Settle Action (Available when reveal closes) */}
                    {(auction.phase === 'reveal' || auction.phase === 'commit') && revealRemainingMs === 0 && (
                      <div className="auction-actions">
                        <button
                          type="button"
                          className="button-primary"
                          disabled={busy}
                          onClick={() => handleSettleBatch(auction)}
                        >
                          Clear Auction (Uniform Price)
                        </button>
                      </div>
                    )}

                    {auction.phase === 'settled' && (
                      <p style={{ color: '#22c55e', fontSize: '0.875rem' }}>
                        Auction Settled! Remaining supply: {auction.qty_remaining}
                      </p>
                    )}
                  </article>
                )
              })}
            </div>
          </section>

          <section className="auction-panel">
            <h3>Create a Batch Auction</h3>
            {user ? (
              <form className="auction-form" onSubmit={handleCreateBatch}>
                <label>
                  Market
                  <select
                    value={batchForm.marketId}
                    onChange={(e) => setBatchForm((c) => ({ ...c, marketId: e.target.value }))}
                  >
                    {markets.map((m) => (
                      <option key={m.config.id} value={m.config.id}>
                        {m.config.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label>
                  Total supply (passes)
                  <input
                    type="number"
                    min="1"
                    value={batchForm.qty}
                    onChange={(e) => setBatchForm((c) => ({ ...c, qty: e.target.value }))}
                  />
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
                  {busy ? 'Working…' : 'Publish Batch Auction'}
                </button>
              </form>
            ) : (
              <div className="auction-guest">
                <p>Log in to reserve passes and publish a batch auction.</p>
                <Link to="/login" state={{ from: '/auctions' }}>
                  Log in
                </Link>
              </div>
            )}
          </section>
        </>
      )}
    </div>
  )
}
