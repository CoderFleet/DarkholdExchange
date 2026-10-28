import { useCallback, useEffect, useMemo, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router-dom'
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

  const { marketId: marketIdParam } = useParams()
  const routeMarketId = useMemo(() => {
    const parsed = Number(marketIdParam)
    return Number.isInteger(parsed) && parsed > 0 ? parsed : null
  }, [marketIdParam])
  const selectedMarket = useMemo(
    () => markets.find((market) => market.id === routeMarketId) ?? null,
    [markets, routeMarketId],
  )
  const selectedMarketId = selectedMarket?.id ?? null

  const [balances, setBalances] = useState([])
  const [openOrders, setOpenOrders] = useState([])
  const [marketsLoaded, setMarketsLoaded] = useState(false)
  // Private pages are behind RequireRole, so this is always a real signed-in user.
  const authUser = useAuthStore((state) => state.user)
  const userId = authUser?.user_id ?? null
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
    if (id == null) return
    const [nextBalances, nextOrders] = await Promise.all([
      getBalances(id),
      getOpenOrders(),
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
        setMarketsLoaded(true)
      })
      .catch((error) => {
        if (!cancelled) setBootError(error.message)
      })
    return () => {
      cancelled = true
    }
  }, [hydrateMarkets])

  useEffect(() => {
    if (userId == null) return undefined
    let cancelled = false
    Promise.all([getBalances(userId), getOpenOrders()])
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

  // Runs on first load and whenever who we are changes (log in, log out).
  // Points the wallet and history pages at that user and loads their wallet.
  useEffect(() => {
    if (userId == null) {
      useWalletStore.getState().resetAccount()
      return
    }
    useWalletStore.getState().setUserId(userId)
  }, [userId])

  useEffect(() => {
    if (activeView !== 'trade' || selectedMarketId == null) return undefined
    let cancelled = false
    loadTrades(selectedMarketId).catch((error) => {
      if (!cancelled) setToast({ kind: 'error', text: error.message })
    })
    return () => {
      cancelled = true
    }
  }, [activeView, selectedMarketId, loadTrades])

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
    if (activeView !== 'trade' || selectedMarketId == null || userId == null) {
      disconnectFeed()
      return undefined
    }
    connectFeed(userId, selectedMarketId)
    // The worker closes an old socket when connect() is called for a new
    // market/user, and this effect releases it when the terminal unmounts.
    return () => disconnectFeed()
  }, [activeView, userId, selectedMarketId, connectFeed, disconnectFeed])

  useEffect(() => {
    if (!toast) return undefined
    const timer = setTimeout(() => setToast(null), 4500)
    return () => clearTimeout(timer)
  }, [toast])

  const selectedSnapshot = selectedMarket ? snapshots[selectedMarket.id] : null
  const selectedTrades = selectedMarket
    ? tradesByMarket[selectedMarket.id] ?? []
    : []

  const runCommand = async (request) => {
    if (userId == null) {
      setToast({ kind: 'error', text: 'Log in to trade.' })
      return null
    }
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
      return await runCommand(() => postCommand(command))
    } finally {
      setSubmitting(false)
    }
  }

  const handleCancel = async (orderId) => {
    setCancelling(orderId)
    try {
      await runCommand(() =>
        cancelOrderRequest(orderId, nextClientOrderId()),
      )
    } finally {
      setCancelling(null)
    }
  }

  return (
    <div className="app-shell">
      {activeView === 'trade' && (
        <MarketToolbar
          connection={feedStatus}
          resyncing={resyncing}
          account={authUser}
          marketName={selectedMarket?.name}
        />
      )}

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
            <div className="loading-panel market-missing">
              {bootError ? (
                <>
                  <p>Gateway unavailable.</p>
                  <Link to="/markets">Back to markets</Link>
                </>
              ) : !marketsLoaded ? (
                'Connecting to the exchange…'
              ) : (
                <>
                  <p>Market not found.</p>
                  <Link to="/markets">Browse the markets</Link>
                </>
              )}
            </div>
          )}
        </>
      )}

      {toast && <Toast toast={toast} onClose={() => setToast(null)} />}
    </div>
  )
}

function MarketToolbar({ connection, resyncing, account, marketName }) {
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
    <div className="market-toolbar">
      <nav className="market-breadcrumb" aria-label="Breadcrumb">
        <Link to="/markets">Markets</Link>
        <span aria-hidden="true">/</span>
        <span>{marketName ?? 'Terminal'}</span>
      </nav>
      <div className="market-toolbar-controls">
        <div className={`connection ${tone}`}>
          <span className="dot" />
          {label}
        </div>
        {account ? (
          <span className="identity">
            Signed in as {account.username}
            <span className="identity-role">{account.role}</span>
          </span>
        ) : null}
      </div>
    </div>
  )
}

function MarketTabs({ markets, snapshots, selectedMarketId }) {
  return (
    <nav className="market-tabs" aria-label="Markets">
      {markets.map((market) => {
        const snapshot = snapshots[market.id]
        return (
          <Link
            key={market.id}
            to={`/markets/${market.id}`}
            className={`market-tab ${
              selectedMarketId === market.id ? 'active' : ''
            }`}
            aria-current={selectedMarketId === market.id ? 'page' : undefined}
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
          </Link>
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
          className="collar"
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

  const escrowPublicKey = useWalletStore((state) => state.escrowPublicKey)
  const escrowLoading = useWalletStore((state) => state.escrowLoading)
  const registerSigningKey = useWalletStore((state) => state.registerSigningKey)

  const [side, setSide] = useState(Side.Buy)
  const [orderType, setOrderType] = useState('limit')
  const [price, setPrice] = useState(String(initialPrice))
  const [qty, setQty] = useState('1')
  const [useEscrow, setUseEscrow] = useState(false)
  const [keyNotice, setKeyNotice] = useState(null)

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
  const sellerNeedsKey = market.escrow_enabled && side === Side.Sell && !escrowPublicKey

  const handleRegisterKey = async () => {
    try {
      setKeyNotice(null)
      await registerSigningKey()
      setKeyNotice('Seller key registered successfully! You can now place sell orders.')
    } catch (err) {
      setKeyNotice(`Failed to register key: ${err.message}`)
    }
  }

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

        {sellerNeedsKey && (
          <div
            style={{
              background: '#451a03',
              border: '1px solid #b45309',
              borderRadius: '6px',
              padding: '0.75rem',
              marginBottom: '1rem',
              fontSize: '0.85rem',
              color: '#fef3c7',
            }}
          >
            <strong style={{ display: 'block', marginBottom: '0.25rem' }}>Seller Signing Key Required</strong>
            <span>This market uses escrow settlement. Sellers must register a delivery signing key before posting asks.</span>
            <div style={{ marginTop: '0.5rem' }}>
              <button
                type="button"
                className="ghost-button"
                onClick={handleRegisterKey}
                disabled={escrowLoading}
                style={{ fontSize: '0.8rem', padding: '0.3rem 0.6rem' }}
              >
                {escrowLoading ? 'Registering…' : '🔑 Register Seller Key Now'}
              </button>
            </div>
            {keyNotice && <div style={{ marginTop: '0.5rem', color: '#6ee7b7' }}>{keyNotice}</div>}
          </div>
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
