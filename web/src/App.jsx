import { useCallback, useEffect, useMemo, useState } from 'react'
import { useNavigate } from 'react-router-dom'
import {
  nextClientOrderId,
  placeLimit,
  placeMarket,
  Side,
} from './protocol'
import {
  cancelOrderRequest,
  getBalances,
  getMarkets,
  getOpenOrders,
  getTrades,
  postCommand,
} from './api'
import {
  formatMoney,
  formatPercent,
  formatPrice,
  formatQty,
  marketBounds,
  statusText,
} from './format'
import { useAuthStore } from './stores/useAuthStore'
import { useWalletStore } from './stores/useWalletStore'
import { useMarketFeedStore } from './stores/useMarketFeedStore'
import { WalletView } from './components/WalletView'
import { HistoryView } from './components/HistoryView'
import { DepthChart, PriceChart } from './components/MarketCharts'
import { SpeedPanel } from './components/SpeedPanel'
import { createLatencySample } from './lib/latency'
import './App.css'

const TRADER_IDS = [1, 2, 3, 4, 5, 6]
const MAX_LATENCY_SAMPLES = 50
const BOOK_LEVELS = 14

function App({ initialView = 'trade' }) {
  const activeView = initialView
  const wallet = useWalletStore()
  const updateWalletFromBalances = useWalletStore(
    (state) => state.updateFromBalances,
  )

  const markets = useMarketFeedStore((state) => state.markets)
  const snapshots = useMarketFeedStore((state) => state.snapshots)
  const tradesByMarket = useMarketFeedStore((state) => state.trades)
  const feedStatus = useMarketFeedStore((state) => state.status)
  const resyncing = useMarketFeedStore((state) => state.resyncing)
  const lastTrade = useMarketFeedStore((state) => state.lastTrade)
  const hydrateMarkets = useMarketFeedStore((state) => state.hydrate)
  const seedTrades = useMarketFeedStore((state) => state.seedTrades)
  const connectFeed = useMarketFeedStore((state) => state.connect)
  const disconnectFeed = useMarketFeedStore((state) => state.disconnect)

  const [balances, setBalances] = useState([])
  const [openOrders, setOpenOrders] = useState([])
  const [selectedMarketId, setSelectedMarketId] = useState(null)
  // Logged in: you are who you logged in as. Not logged in (dev only): the Identity dropdown picks.
  const authUser = useAuthStore((state) => state.user)
  const [devUserId, setDevUserId] = useState(1)
  const userId = authUser ? authUser.user_id : devUserId
  const [toast, setToast] = useState(null)
  const [submitting, setSubmitting] = useState(false)
  const [cancelling, setCancelling] = useState(null)
  const [bootError, setBootError] = useState(null)
  const [latencySamples, setLatencySamples] = useState([])
  const [lastLatency, setLastLatency] = useState(null)

  const recordLatency = useCallback((sample) => {
    setLastLatency(sample)
    setLatencySamples((current) => [
      ...current.slice(-(MAX_LATENCY_SAMPLES - 1)),
      sample,
    ])
  }, [])

  const clearLatency = useCallback(() => {
    setLastLatency(null)
    setLatencySamples([])
  }, [])

  const loadAccount = useCallback(async (id) => {
    const [nextBalances, nextOrders] = await Promise.all([
      getBalances(id),
      getOpenOrders(id),
    ])
    setBalances(nextBalances)
    setOpenOrders(nextOrders)
    updateWalletFromBalances(
      nextBalances,
      useMarketFeedStore.getState().markets,
    )
    // The balances endpoint does not include escrow cash or pending escrow
    // passes, so refresh the wallet projection after a fill.
    await useWalletStore.getState().fetchWallet()
  }, [updateWalletFromBalances])

  const loadTrades = useCallback(async (marketId) => {
    if (marketId == null) return
    const nextTrades = await getTrades(marketId, 60)
    seedTrades(marketId, nextTrades)
  }, [seedTrades])

  useEffect(() => {
    let cancelled = false
    getMarkets()
      .then((views) => {
        if (cancelled) return
        hydrateMarkets(views)
        setSelectedMarketId((current) => current ?? views[0]?.config.id ?? null)
      })
      .catch((error) => {
        if (!cancelled) setBootError(error.message)
      })
    return () => {
      cancelled = true
    }
  }, [hydrateMarkets])

  useEffect(() => {
    let cancelled = false
    Promise.all([getBalances(userId), getOpenOrders(userId)])
      .then(([nextBalances, nextOrders]) => {
        if (cancelled) return
        setBalances(nextBalances)
        setOpenOrders(nextOrders)
      })
      .catch((error) => {
        if (!cancelled) setToast({ kind: 'error', text: error.message })
      })
    return () => {
      cancelled = true
    }
  }, [userId])

  // Runs on first load and whenever who we are changes (log in, log out, dropdown).
  // Points the wallet and history pages at that user and loads their wallet.
  useEffect(() => {
    useWalletStore.getState().setUserId(userId)
  }, [userId])

  useEffect(() => {
    if (selectedMarketId == null) return undefined
    let cancelled = false
    loadTrades(selectedMarketId).catch((error) => {
      if (!cancelled) setToast({ kind: 'error', text: error.message })
    })
    return () => {
      cancelled = true
    }
  }, [selectedMarketId, loadTrades])

  useEffect(() => {
    if (!lastTrade) return
    if (
      lastTrade.maker_user_id === userId ||
      lastTrade.taker_user_id === userId
    ) {
      // The feed is an external system; a fill is the event that invalidates the
      // REST account snapshot. The request itself updates state asynchronously.
      // eslint-disable-next-line react-hooks/set-state-in-effect
      loadAccount(userId).catch(() => {})
    }
  }, [lastTrade, userId, loadAccount])

  useEffect(() => {
    if (selectedMarketId == null) return
    connectFeed(userId, selectedMarketId)

    // The worker closes an old socket when connect() is called for a new
    // market/user, so this effect only owns the logical selection.
  }, [userId, selectedMarketId, connectFeed])

  useEffect(() => () => disconnectFeed(), [disconnectFeed])

  useEffect(() => {
    if (!toast) return undefined
    const timer = setTimeout(() => setToast(null), 4500)
    return () => clearTimeout(timer)
  }, [toast])

  const selectedMarket = useMemo(
    () => markets.find((market) => market.id === selectedMarketId) ?? null,
    [markets, selectedMarketId],
  )
  const selectedSnapshot = selectedMarket ? snapshots[selectedMarket.id] : null
  const selectedTrades = selectedMarket
    ? tradesByMarket[selectedMarket.id] ?? []
    : []

  const runCommand = async (request) => {
    const sentAt = performance.now()
    try {
      const response = await request()
      const acknowledgedAt = performance.now()
      setToast({
        kind: response.status === 'Rejected' ? 'error' : 'success',
        text: statusText(response),
      })
      if (response.fills?.length) {
        for (const fill of response.fills) seedTrades(fill.market_id, [fill])
      }
      await loadAccount(userId)
      if (selectedMarketId != null) await loadTrades(selectedMarketId)

      window.requestAnimationFrame(() => {
        window.requestAnimationFrame(() => {
          recordLatency(
            createLatencySample(sentAt, acknowledgedAt, performance.now()),
          )
        })
      })
      return response
    } catch (error) {
      setToast({ kind: 'error', text: error.message })
      return null
    }
  }

  const submit = async (command) => {
    setSubmitting(true)
    try {
      return await runCommand(() => postCommand(command, userId))
    } finally {
      setSubmitting(false)
    }
  }

  const handleCancel = async (orderId) => {
    setCancelling(orderId)
    try {
      await runCommand(() =>
        cancelOrderRequest(orderId, nextClientOrderId(), userId),
      )
    } finally {
      setCancelling(null)
    }
  }

  const handleUserChange = (event) => {
    const nextId = Number(event.target.value)
    setDevUserId(nextId)
    wallet.setUserId(nextId)
    setBalances([])
    setOpenOrders([])
  }

  return (
    <div className="app-shell">
      <Header
        connection={feedStatus}
        resyncing={resyncing}
        userId={userId}
        account={authUser}
        onUserChange={handleUserChange}
      />

      {bootError && <div className="banner error">{bootError}</div>}

      {activeView === 'speed' ? (
        <SpeedView
          samples={latencySamples}
          lastSample={lastLatency}
          onClear={clearLatency}
        />
      ) : activeView === 'wallet' ? (
        <WalletView
          wallet={wallet}
          onTopup={(amt) => wallet.topup(amt)}
          onRefresh={() => wallet.fetchWallet()}
        />
      ) : activeView === 'history' ? (
        <HistoryView
          wallet={wallet}
          onRefresh={() => wallet.fetchHistory()}
        />
      ) : (
        <>
          <MarketTabs
            markets={markets}
            snapshots={snapshots}
            selectedMarketId={selectedMarketId}
            onSelect={setSelectedMarketId}
          />

          {selectedMarket ? (
            <>
              <MarketHeader market={selectedMarket} snapshot={selectedSnapshot} />

              <div className="market-main-grid">
                <OrderBook market={selectedMarket} snapshot={selectedSnapshot} />

                <div className="market-chart-stack">
                  <PriceChart market={selectedMarket} trades={selectedTrades} />
                  <DepthChart market={selectedMarket} snapshot={selectedSnapshot} />
                </div>

                <OrderTicket
                  key={selectedMarket.id}
                  market={selectedMarket}
                  snapshot={selectedSnapshot}
                  submitting={submitting}
                  onSubmit={submit}
                />
                <Balances
                  market={selectedMarket}
                  balances={balances}
                  userId={userId}
                />
                <OpenOrders
                  market={selectedMarket}
                  orders={openOrders}
                  cancelling={cancelling}
                  onCancel={handleCancel}
                />
              </div>

              <div className="market-secondary-grid">
                <TradeTape market={selectedMarket} trades={selectedTrades} />
                <SpeedPanel
                  samples={latencySamples}
                  lastSample={lastLatency}
                />
              </div>
            </>
          ) : (
            <div className="loading-panel">
              {bootError ? 'Gateway unavailable.' : 'Connecting to the exchange…'}
            </div>
          )}
        </>
      )}

      <Footer />
      {toast && <Toast toast={toast} onClose={() => setToast(null)} />}
    </div>
  )
}

function Header({ connection, resyncing, userId, account, onUserChange }) {
  const label = resyncing
    ? 'Resyncing'
    : connection === 'live'
      ? 'Live'
      : connection === 'connecting'
        ? 'Connecting'
        : connection === 'reconnecting'
          ? 'Reconnecting'
          : 'Offline'
  const tone =
    connection === 'live' && !resyncing
      ? 'live'
      : connection === 'offline'
        ? 'offline'
        : 'connecting'

  return (
    <header className="topbar">
      <div className="brand">
        <div className="brand-mark">DX</div>
        <div>
          <span className="eyebrow">Darkhold Exchange</span>
          <h1>Pass market terminal</h1>
          <p>Live matching · integer ticks · MessagePack feed</p>
        </div>
      </div>
      <div className="topbar-controls">
        <div className={`connection ${tone}`}>
          <span className="dot" />
          {label}
        </div>
        {account ? (
          <span className="identity">Signed in as {account.username}</span>
        ) : (
          <label className="identity">
            Identity
            <select value={userId} onChange={onUserChange}>
              {TRADER_IDS.map((id) => (
                <option key={id} value={id}>
                  Trader #{id}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
    </header>
  )
}

function MarketTabs({ markets, snapshots, selectedMarketId, onSelect }) {
  return (
    <nav className="market-tabs" aria-label="Markets">
      {markets.map((market) => {
        const snapshot = snapshots[market.id]
        return (
          <button
            key={market.id}
            type="button"
            className={`market-tab ${
              selectedMarketId === market.id ? 'active' : ''
            }`}
            onClick={() => onSelect(market.id)}
          >
            <span className="market-tab-top">
              <span>{market.name}</span>
              <span className="market-tab-id">M{market.id}</span>
            </span>
            <span className="market-tab-meta">
              {formatMoney(market.face_value_minor)} face ·{' '}
              {formatPercent(market.taker_fee_bps)} fee
            </span>
            <span
              className={`market-tab-last ${
                snapshot?.last_trade_price ? 'has-trade' : ''
              }`}
            >
              {snapshot?.last_trade_price
                ? formatPrice(market, snapshot.last_trade_price)
                : 'No prints yet'}
            </span>
          </button>
        )
      })}
    </nav>
  )
}

function MarketHeader({ market, snapshot }) {
  const { floor, cap } = marketBounds(market)
  const bestBid = snapshot?.bids?.[0]?.price_ticks
  const bestAsk = snapshot?.asks?.[0]?.price_ticks
  const last = snapshot?.last_trade_price
  const spread = bestAsk != null && bestBid != null ? bestAsk - bestBid : null

  return (
    <section className="market-header panel">
      <div className="market-title">
        <span className="eyebrow">
          Market {market.id} · Event {market.event_id}
        </span>
        <h2>{market.name}</h2>
        <p>
          {market.escrow_enabled ? 'Escrow enabled' : 'Spot settlement'} ·{' '}
          {market.holding_limit} pass holding limit
        </p>
      </div>
      <div className="stat-grid">
        <Stat
          label="Last print"
          value={last ? formatPrice(market, last) : '—'}
          className="accent"
        />
        <Stat
          label="Best bid"
          value={bestBid ? formatPrice(market, bestBid) : '—'}
          className="positive"
        />
        <Stat
          label="Best ask"
          value={bestAsk ? formatPrice(market, bestAsk) : '—'}
          className="negative"
        />
        <Stat
          label="Spread"
          value={spread != null ? `${formatQty(spread)} ticks` : '—'}
        />
        <Stat
          label="Collar"
          value={`${formatPrice(market, floor)} – ${formatPrice(market, cap)}`}
        />
        <Stat label="Taker fee" value={formatPercent(market.taker_fee_bps)} />
      </div>
    </section>
  )
}

function Stat({ label, value, className = '' }) {
  return (
    <div className={`stat ${className}`}>
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  )
}

function OrderBook({ market, snapshot }) {
  const bids = snapshot?.bids?.slice(0, BOOK_LEVELS) ?? []
  const asks = snapshot?.asks?.slice(0, BOOK_LEVELS) ?? []
  const maxQty = Math.max(
    1,
    ...bids.map((level) => Number(level.qty)),
    ...asks.map((level) => Number(level.qty)),
  )
  const bestBid = bids[0]?.price_ticks
  const bestAsk = asks[0]?.price_ticks
  const spread = bestAsk != null && bestBid != null ? bestAsk - bestBid : null
  const askRows = [...asks].reverse()
  const askFillers = Array.from({
    length: Math.max(0, BOOK_LEVELS - askRows.length),
  })
  const bidFillers = Array.from({
    length: Math.max(0, BOOK_LEVELS - bids.length),
  })

  return (
    <section className="panel order-book">
      <PanelHeader
        title="Order book"
        meta={`Book seq ${snapshot?.book_seq ?? '—'}`}
      />
      <div className="book-head">
        <span>Price</span>
        <span>Qty</span>
      </div>
      <div className="book-side asks">
        {asks.length === 0 && (
          <span className="book-empty-note">No asks resting</span>
        )}
        {askFillers.map((_, index) => (
          <div
            className="book-row placeholder"
            aria-hidden="true"
            key={`ask-filler-${index}`}
          />
        ))}
        {askRows.map((level) => (
          <BookRow
            key={`ask-${level.price_ticks}`}
            market={market}
            level={level}
            maxQty={maxQty}
            side="ask"
          />
        ))}
      </div>
      <div className="spread-row">
        <span>Spread</span>
        <strong>{spread != null ? `${formatQty(spread)} ticks` : '—'}</strong>
        <span>
          {spread != null
            ? formatMoney(spread * Number(market.tick_size_minor))
            : ''}
        </span>
      </div>
      <div className="book-side bids">
        {bids.length === 0 && (
          <span className="book-empty-note">No bids resting</span>
        )}
        {bids.map((level) => (
          <BookRow
            key={`bid-${level.price_ticks}`}
            market={market}
            level={level}
            maxQty={maxQty}
            side="bid"
          />
        ))}
        {bidFillers.map((_, index) => (
          <div
            className="book-row placeholder"
            aria-hidden="true"
            key={`bid-filler-${index}`}
          />
        ))}
      </div>
      <div className="book-footer">
        <span>
          Best bid{' '}
          <b>{bestBid != null ? formatPrice(market, bestBid) : '—'}</b>
        </span>
        <span>
          Best ask{' '}
          <b>{bestAsk != null ? formatPrice(market, bestAsk) : '—'}</b>
        </span>
      </div>
    </section>
  )
}

function BookRow({ market, level, maxQty, side }) {
  const width = `${Math.min(100, (Number(level.qty) / maxQty) * 100)}%`
  return (
    <div className={`book-row ${side}`}>
      <span className="depth-bar" style={{ width }} />
      <span className="book-price">{formatPrice(market, level.price_ticks)}</span>
      <span className="book-qty">{formatQty(level.qty)}</span>
    </div>
  )
}

function TradeTape({ market, trades }) {
  const navigate = useNavigate()
  const visible = trades.slice(-40).reverse()

  return (
    <section className="panel trade-tape">
      <PanelHeader title="Trade tape" meta={`${trades.length} prints`} />
      <div className="tape-head">
        <span>Price</span>
        <span>Qty</span>
        <span>Taker</span>
      </div>
      <div className="tape-list">
        {visible.length === 0 && (
          <div className="empty-row">No trades in this market yet.</div>
        )}
        {visible.map((fill) => {
          const takerSide = fill.maker_side === 'Buy' ? 'Sell' : 'Buy'
          const isEscrow = fill.escrow_id != null
          return (
            <div
              className={`tape-row${isEscrow ? ' escrow' : ''}`}
              key={fill.trade_id}
              style={isEscrow ? { cursor: 'pointer' } : {}}
              onClick={isEscrow ? () => navigate(`/trade/${fill.escrow_id}`) : undefined}
              title={isEscrow ? `Escrow #${fill.escrow_id} — click to inspect` : undefined}
            >
              <span
                className={
                  takerSide === 'Buy' ? 'positive' : 'negative'
                }
              >
                {formatPrice(market, fill.price_ticks)}
              </span>
              <span>{formatQty(fill.qty)}</span>
              <span className={`side-tag ${takerSide.toLowerCase()}`}>
                {takerSide}
              </span>
              {isEscrow && (
                <span
                  style={{
                    fontSize: '0.65rem',
                    background: '#78350f',
                    color: '#fef3c7',
                    borderRadius: '3px',
                    padding: '1px 4px',
                    marginLeft: '4px',
                  }}
                >
                  ESC
                </span>
              )}
            </div>
          )
        })}
      </div>
    </section>
  )
}

function OpenOrders({ market, orders, cancelling, onCancel }) {
  const marketOrders = orders.filter((order) => order.market_id === market.id)

  return (
    <section className="panel open-orders">
      <PanelHeader
        title="Open orders"
        meta={`${marketOrders.length} in market`}
      />
      {marketOrders.length === 0 ? (
        <div className="empty-row">
          No resting orders. Place a limit order to join the book.
        </div>
      ) : (
        <div className="orders-list">
          {marketOrders.map((order) => (
            <div className="order-row" key={order.order_id}>
              <span className={`side-tag ${order.side.toLowerCase()}`}>
                {order.side}
              </span>
              <span className="mono">
                {formatPrice(market, order.price_ticks)}
              </span>
              <span className="muted">
                {formatQty(order.remaining)} / {formatQty(order.qty)}
              </span>
              <button
                className="ghost-button"
                type="button"
                onClick={() => onCancel(order.order_id)}
                disabled={cancelling === order.order_id}
              >
                {cancelling === order.order_id ? '…' : 'Cancel'}
              </button>
            </div>
          ))}
        </div>
      )}
    </section>
  )
}

function OrderTicket({ market, snapshot, submitting, onSubmit }) {
  const bounds = marketBounds(market)
  const bestBid = snapshot?.bids?.[0]?.price_ticks
  const bestAsk = snapshot?.asks?.[0]?.price_ticks
  const mid =
    bestBid != null && bestAsk != null
      ? Math.round((bestBid + bestAsk) / 2)
      : Math.round((bounds.floor + bounds.cap) / 2)
  const initialPrice = Math.min(
    bounds.cap,
    Math.max(bounds.floor, snapshot?.last_trade_price ?? mid),
  )

  const [side, setSide] = useState(Side.Buy)
  const [orderType, setOrderType] = useState('limit')
  const [price, setPrice] = useState(String(initialPrice))
  const [qty, setQty] = useState('1')
  const [useEscrow, setUseEscrow] = useState(false)

  const priceTicks = Number(price)
  const quantity = Number(qty)
  const protectTicks = side === Side.Buy ? bounds.cap : bounds.floor
  const effectivePrice = orderType === 'limit' ? priceTicks : protectTicks
  const notional =
    Number.isInteger(effectivePrice) && Number.isInteger(quantity)
      ? effectivePrice * Number(market.tick_size_minor) * quantity
      : 0
  const fee = Math.ceil(
    (notional * Number(market.taker_fee_bps)) / 10_000,
  )
  const priceInvalid =
    orderType === 'limit' &&
    (!Number.isInteger(priceTicks) ||
      priceTicks < bounds.floor ||
      priceTicks > bounds.cap)
  const qtyInvalid = !Number.isInteger(quantity) || quantity < 1
  const disabled = submitting || priceInvalid || qtyInvalid

  // Escrow is only available when the market has it enabled and taker is buying
  const escrowAvailable = market.escrow_enabled && side === Side.Buy

  const handleSubmit = async (event) => {
    event.preventDefault()
    if (disabled) return
    const escrow = escrowAvailable && useEscrow
    const command =
      orderType === 'limit'
        ? placeLimit({
            clientOrderId: nextClientOrderId(),
            marketId: market.id,
            side,
            priceTicks,
            qty: quantity,
            escrow,
          })
        : placeMarket({
            clientOrderId: nextClientOrderId(),
            marketId: market.id,
            side,
            qty: quantity,
            protectPriceTicks: protectTicks,
            escrow,
          })
    const response = await onSubmit(command)
    if (response && response.status !== 'Rejected') setQty('1')
  }

  return (
    <section className="panel order-ticket">
      <PanelHeader
        title="Order ticket"
        meta={`Holding limit ${market.holding_limit}`}
      />
      <form onSubmit={handleSubmit}>
        <div className="segmented">
          <button
            type="button"
            className={side === Side.Buy ? 'active buy' : ''}
            onClick={() => setSide(Side.Buy)}
          >
            Buy
          </button>
          <button
            type="button"
            className={side === Side.Sell ? 'active sell' : ''}
            onClick={() => setSide(Side.Sell)}
          >
            Sell
          </button>
        </div>
        <div className="segmented subtle">
          <button
            type="button"
            className={orderType === 'limit' ? 'active' : ''}
            onClick={() => setOrderType('limit')}
          >
            Limit
          </button>
          <button
            type="button"
            className={orderType === 'market' ? 'active' : ''}
            onClick={() => setOrderType('market')}
          >
            Market
          </button>
        </div>

        {orderType === 'limit' ? (
          <label className="field">
            <span>
              Price <em>ticks</em>
            </span>
            <input
              type="number"
              min={bounds.floor}
              max={bounds.cap}
              step="1"
              value={price}
              onChange={(event) => setPrice(event.target.value)}
            />
            <small>
              {priceInvalid
                ? `Collar ${bounds.floor}–${bounds.cap} ticks`
                : `${formatPrice(market, priceTicks)} per pass`}
            </small>
          </label>
        ) : (
          <div className="protect-box">
            <span>Protected at {formatPrice(market, protectTicks)}</span>
            <small>
              {side === Side.Buy ? 'Maximum buy price' : 'Minimum sell price'} =
              collar {side === Side.Buy ? 'cap' : 'floor'}
            </small>
          </div>
        )}

        <label className="field">
          <span>
            Quantity <em>passes</em>
          </span>
          <input
            type="number"
            min="1"
            step="1"
            value={qty}
            onChange={(event) => setQty(event.target.value)}
          />
        </label>

        {escrowAvailable && (
          <label
            className="field"
            style={{ flexDirection: 'row', alignItems: 'center', gap: '0.5rem', cursor: 'pointer' }}
          >
            <input
              type="checkbox"
              checked={useEscrow}
              onChange={(e) => setUseEscrow(e.target.checked)}
              style={{ width: 'auto', accentColor: '#f59e0b' }}
            />
            <span style={{ fontSize: '0.85rem' }}>
              Escrow <em style={{ color: '#9ca3af' }}>— hold funds until you accept the pass</em>
            </span>
          </label>
        )}

        <div className="estimate">
          <div>
            <span>{orderType === 'market' ? 'Max notional' : 'Notional'}</span>
            <b>{formatMoney(notional)}</b>
          </div>
          <div>
            <span>Taker fee ({formatPercent(market.taker_fee_bps)})</span>
            <b>{formatMoney(fee)}</b>
          </div>
          <div className="total">
            <span>Total if filled</span>
            <b>{formatMoney(notional + fee)}</b>
          </div>
        </div>

        <button
          className={`submit-button ${side.toLowerCase()}`}
          type="submit"
          disabled={disabled}
        >
          {submitting
            ? 'Submitting…'
            : `${side} ${formatQty(quantity || 0)} ${
                quantity === 1 ? 'pass' : 'passes'
              }${escrowAvailable && useEscrow ? ' (Escrow)' : ''}`}
        </button>
        <p className="fine-print">
          Maker fills pay no fee. Taker fee is charged on matched quantity.
        </p>
      </form>
    </section>
  )
}

function Balances({ market, balances, userId }) {
  const cash = balances.find((balance) => balance.asset.kind === 'cash')
  const pass = balances.find(
    (balance) =>
      balance.asset.kind === 'pass' && balance.asset.market_id === market.id,
  )

  return (
    <section className="panel balances">
      <PanelHeader title="Balances" meta={`Trader #${userId}`} />
      <div className="balance-card">
        <span className="balance-label">Cash</span>
        <strong>{formatMoney(cash?.available ?? 0)}</strong>
        <span className="balance-held">{formatMoney(cash?.held ?? 0)} held</span>
      </div>
      <div className="balance-card pass">
        <span className="balance-label">{market.name} passes</span>
        <strong>{formatQty(pass?.available ?? 0)}</strong>
        <span className="balance-held">{formatQty(pass?.held ?? 0)} held</span>
      </div>
      <div className="balance-note">
        Available + held stays backed by the double-entry ledger.
      </div>
    </section>
  )
}

function PanelHeader({ title, meta }) {
  return (
    <div className="panel-header">
      <h3>{title}</h3>
      {meta && <span className="panel-meta">{meta}</span>}
    </div>
  )
}

function SpeedView({ samples, lastSample, onClear }) {
  return (
    <div className="speed-view">
      <section className="panel speed-overview">
        <div>
          <span className="eyebrow">T1 · Live telemetry</span>
          <h2>Speed and latency</h2>
          <p>
            Every order is measured from the browser send to the REST
            acknowledgement, then to the painted result. The gateway adds its own
            queue and matching-core histograms.
          </p>
        </div>
        <div className="speed-overview-tags">
          <span>performance.now()</span>
          <span>single-writer core</span>
          <span>p50 / p99</span>
        </div>
      </section>

      <SpeedPanel
        variant="page"
        samples={samples}
        lastSample={lastSample}
        onClear={onClear}
      />
    </div>
  )
}

function Footer() {
  return (
    <footer className="footer">
      <span>Darkhold Exchange · Webster 2026</span>
      <span>Single-writer core · i64 minor units · no floats</span>
    </footer>
  )
}

function Toast({ toast, onClose }) {
  return (
    <div className={`toast ${toast.kind}`} role="status">
      <span>{toast.text}</span>
      <button type="button" onClick={onClose} aria-label="Dismiss">
        ×
      </button>
    </div>
  )
}

export default App
