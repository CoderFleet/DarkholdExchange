import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  cancelOrder as cancelOrderCommand,
  decodeServerMessage,
  encodeClientMessage,
  nextClientOrderId,
  placeLimit,
  placeMarket,
  Side,
  subscribe,
} from './protocol'
import {
  getBalances,
  getMarkets,
  getOpenOrders,
  getTrades,
  postCommand,
  websocketUrl,
} from './api'
import {
  formatMoney,
  formatPercent,
  formatPrice,
  formatQty,
  marketBounds,
  statusText,
} from './format'
import './App.css'

const TRADER_IDS = [1, 2, 3, 4, 5, 6]

function App() {
  const [markets, setMarkets] = useState([])
  const [snapshots, setSnapshots] = useState({})
  const [tradesByMarket, setTradesByMarket] = useState({})
  const [balances, setBalances] = useState([])
  const [openOrders, setOpenOrders] = useState([])
  const [selectedMarketId, setSelectedMarketId] = useState(null)
  const [userId, setUserId] = useState(1)
  const [connection, setConnection] = useState('connecting')
  const [toast, setToast] = useState(null)
  const [submitting, setSubmitting] = useState(false)
  const [cancelling, setCancelling] = useState(null)
  const [bootError, setBootError] = useState(null)

  const loadAccount = useCallback(async (id) => {
    const [nextBalances, nextOrders] = await Promise.all([
      getBalances(id),
      getOpenOrders(id),
    ])
    setBalances(nextBalances)
    setOpenOrders(nextOrders)
  }, [])

  const loadTrades = useCallback(async (marketId) => {
    if (marketId == null) return
    const nextTrades = await getTrades(marketId, 60)
    setTradesByMarket((current) => ({ ...current, [marketId]: nextTrades }))
  }, [])

  useEffect(() => {
    let cancelled = false
    getMarkets()
      .then((views) => {
        if (cancelled) return
        setMarkets(views.map((view) => view.config))
        setSnapshots(
          Object.fromEntries(views.map((view) => [view.config.id, view.snapshot])),
        )
        setSelectedMarketId((current) => current ?? views[0]?.config.id ?? null)
      })
      .catch((error) => {
        if (!cancelled) setBootError(error.message)
      })
    return () => {
      cancelled = true
    }
  }, [])

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

  useEffect(() => {
    if (selectedMarketId == null) return undefined
    let cancelled = false
    getTrades(selectedMarketId, 60)
      .then((nextTrades) => {
        if (cancelled) return
        setTradesByMarket((current) => ({
          ...current,
          [selectedMarketId]: nextTrades,
        }))
      })
      .catch((error) => {
        if (!cancelled) setToast({ kind: 'error', text: error.message })
      })
    return () => {
      cancelled = true
    }
  }, [selectedMarketId])

  useEffect(() => {
    if (!toast) return undefined
    const timer = setTimeout(() => setToast(null), 4500)
    return () => clearTimeout(timer)
  }, [toast])

  useEffect(() => {
    let stopped = false
    let socket
    let retryTimer

    const connect = () => {
      setConnection('connecting')
      socket = new WebSocket(websocketUrl(userId))
      socket.binaryType = 'arraybuffer'

      socket.onopen = () => {
        if (stopped) return
        setConnection('live')
        socket.send(encodeClientMessage(subscribe({ marketIds: [], depth: 20 })))
      }

      socket.onmessage = (event) => {
        let message
        try {
          message = decodeServerMessage(event.data)
        } catch {
          return
        }

        if (message.type === 'Hello') {
          setMarkets(message.markets)
          setSelectedMarketId(
            (current) => current ?? message.markets[0]?.id ?? null,
          )
        } else if (message.type === 'Snapshot') {
          const { snapshot } = message
          setSnapshots((current) => ({
            ...current,
            [snapshot.market_id]: snapshot,
          }))
        } else if (message.type === 'Trade') {
          const { fill } = message
          setTradesByMarket((current) => {
            const previous = current[fill.market_id] ?? []
            return {
              ...current,
              [fill.market_id]: [...previous, fill].slice(-120),
            }
          })
          if (fill.maker_user_id === userId || fill.taker_user_id === userId) {
            loadAccount(userId).catch(() => {})
          }
        }
      }

      socket.onclose = () => {
        if (stopped) return
        setConnection('offline')
        retryTimer = setTimeout(connect, 1500)
      }

      socket.onerror = () => {
        socket.close()
      }
    }

    connect()
    return () => {
      stopped = true
      clearTimeout(retryTimer)
      socket?.close()
    }
  }, [userId, loadAccount])

  const selectedMarket = useMemo(
    () => markets.find((market) => market.id === selectedMarketId) ?? null,
    [markets, selectedMarketId],
  )
  const selectedSnapshot = selectedMarket ? snapshots[selectedMarket.id] : null
  const selectedTrades = selectedMarket
    ? tradesByMarket[selectedMarket.id] ?? []
    : []

  const submit = async (command) => {
    setSubmitting(true)
    try {
      const response = await postCommand(command, userId)
      setToast({
        kind: response.status === 'Rejected' ? 'error' : 'success',
        text: statusText(response),
      })
      await loadAccount(userId)
      if (selectedMarketId != null) await loadTrades(selectedMarketId)
      return response
    } catch (error) {
      setToast({ kind: 'error', text: error.message })
      return null
    } finally {
      setSubmitting(false)
    }
  }

  const handleCancel = async (orderId) => {
    setCancelling(orderId)
    try {
      await submit(
        cancelOrderCommand({
          clientOrderId: nextClientOrderId(),
          orderId,
        }),
      )
    } finally {
      setCancelling(null)
    }
  }

  const handleUserChange = (event) => {
    setUserId(Number(event.target.value))
    setBalances([])
    setOpenOrders([])
  }

  return (
    <div className="app-shell">
      <Header
        connection={connection}
        userId={userId}
        onUserChange={handleUserChange}
      />

      {bootError && <div className="banner error">{bootError}</div>}

      <MarketTabs
        markets={markets}
        snapshots={snapshots}
        selectedMarketId={selectedMarketId}
        onSelect={setSelectedMarketId}
      />

      {selectedMarket ? (
        <>
          <MarketHeader market={selectedMarket} snapshot={selectedSnapshot} />
          <div className="terminal-grid">
            <OrderBook market={selectedMarket} snapshot={selectedSnapshot} />
            <div className="column-stack">
              <TradeTape market={selectedMarket} trades={selectedTrades} />
              <OpenOrders
                market={selectedMarket}
                orders={openOrders}
                cancelling={cancelling}
                onCancel={handleCancel}
              />
            </div>
            <div className="column-stack">
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
            </div>
          </div>
        </>
      ) : (
        <div className="loading-panel">
          {bootError ? 'Gateway unavailable.' : 'Connecting to the exchange…'}
        </div>
      )}

      <Footer />
      {toast && <Toast toast={toast} onClose={() => setToast(null)} />}
    </div>
  )
}

function Header({ connection, userId, onUserChange }) {
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
        <div className={`connection ${connection}`}>
          <span className="dot" />
          {connection === 'live'
            ? 'Live'
            : connection === 'connecting'
              ? 'Connecting'
              : 'Reconnecting'}
        </div>
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
  const bids = snapshot?.bids?.slice(0, 12) ?? []
  const asks = snapshot?.asks?.slice(0, 12) ?? []
  const maxQty = Math.max(
    1,
    ...bids.map((level) => Number(level.qty)),
    ...asks.map((level) => Number(level.qty)),
  )
  const bestBid = bids[0]?.price_ticks
  const bestAsk = asks[0]?.price_ticks
  const spread = bestAsk != null && bestBid != null ? bestAsk - bestBid : null

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
        {asks.length === 0 && <EmptyRow text="No asks resting" />}
        {[...asks].reverse().map((level) => (
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
        {bids.length === 0 && <EmptyRow text="No bids resting" />}
        {bids.map((level) => (
          <BookRow
            key={`bid-${level.price_ticks}`}
            market={market}
            level={level}
            maxQty={maxQty}
            side="bid"
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
          return (
            <div className="tape-row" key={fill.trade_id}>
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

  const handleSubmit = async (event) => {
    event.preventDefault()
    if (disabled) return
    const command =
      orderType === 'limit'
        ? placeLimit({
            clientOrderId: nextClientOrderId(),
            marketId: market.id,
            side,
            priceTicks,
            qty: quantity,
          })
        : placeMarket({
            clientOrderId: nextClientOrderId(),
            marketId: market.id,
            side,
            qty: quantity,
            protectPriceTicks: protectTicks,
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
              }`}
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

function EmptyRow({ text }) {
  return <div className="empty-row">{text}</div>
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
