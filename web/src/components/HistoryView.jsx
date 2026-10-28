import { useState, useEffect } from 'react'
import { formatMoney, formatQty } from '../format'

export function HistoryView({ wallet, onRefresh }) {
  const [activeTab, setActiveTab] = useState('trades')
  const [sideFilter, setSideFilter] = useState('all')
  const [now, setNow] = useState(0)
  const [escrowBusy, setEscrowBusy] = useState(null)
  const [escrowError, setEscrowError] = useState(null)

  const {
    historyItems,
    statementItems,
    escrowItems,
    escrowPublicKey,
    escrowLoading,
    fetchHistory,
    fetchStatements,
    fetchEscrow,
    registerSigningKey,
    deliverEscrow,
    refundEscrowAction,
  } = wallet

  useEffect(() => {
    if (activeTab === 'trades') {
      const filter = sideFilter === 'all' ? {} : { side: sideFilter }
      fetchHistory(filter)
    } else if (activeTab === 'statements') {
      fetchStatements()
    } else {
      fetchEscrow()
      const timer = setInterval(() => setNow(Date.now()), 1000)
      return () => clearInterval(timer)
    }
  }, [activeTab, sideFilter, fetchHistory, fetchStatements, fetchEscrow])

  const refresh = () => {
    if (activeTab === 'escrow') {
      fetchEscrow()
    } else if (activeTab === 'statements') {
      fetchStatements()
    } else {
      onRefresh()
    }
  }

  const runEscrowAction = async (tradeId, action) => {
    setEscrowBusy(tradeId)
    setEscrowError(null)
    try {
      await action(tradeId)
    } catch (err) {
      setEscrowError(err.message)
    } finally {
      setEscrowBusy(null)
    }
  }

  const formatTime = (ts) => {
    if (!ts) return '—'
    const ms = ts > 1_000_000_000_000_000 ? Math.floor(ts / 1000) : ts
    const d = new Date(ms)
    return d.toLocaleString('en-IN', {
      day: '2-digit',
      month: 'short',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    })
  }

  const formatKind = (kind) => {
    switch (kind) {
      case 'Deposit':
        return 'Deposit Cash'
      case 'IssuePass':
        return 'Pass Issued'
      case 'ReserveCash':
        return 'Reserved Cash'
      case 'ReleaseCash':
        return 'Released Cash'
      default:
        return kind.replace(/([A-Z])/g, ' $1').trim()
    }
  }

  const formatAccount = (account) => {
    // Pass accounts: PassAvail(201, 1) or PassHeld(201, 1)
    const passMatch = account.match(/PassAvail\((\d+),\s*(\d+)\)/)
    if (passMatch) return `Available Passes (Market #${passMatch[2]})`

    const passHeld = account.match(/PassHeld\((\d+),\s*(\d+)\)/)
    if (passHeld) return `Locked Passes for Order (Market #${passHeld[2]})`

    // Cash accounts: CashAvail(201) or CashLocked(201)
    const cashAvail = account.match(/CashAvail\((\d+)\)/)
    if (cashAvail) return 'Available Cash'

    const cashHeld = account.match(/CashLocked\((\d+)\)|CashHeld\((\d+)\)/)
    if (cashHeld) return 'Locked Cash in Orders'

    const escrowCash = account.match(/EscrowCash\((\d+)\)/)
    if (escrowCash) return 'Escrow Protected Cash'

    return account
  }

  const formatAsset = (asset) => {
    if (asset === 'Cash') return 'Rupees (₹)'
    const match = asset.match(/Pass\((\d+)\)/)
    if (match) return `Market #${match[1]} Pass`
    return asset
  }

  return (
    <div className="history-view">
      <div className="history-heading" style={{ marginBottom: '1.25rem' }}>
        <div>
          <span className="eyebrow">Trading Journal & Ledger</span>
          <h2>Activity & Statements</h2>
        </div>
        <p className="history-intro" style={{ margin: '4px 0 0', color: 'var(--muted)', fontSize: '13px', lineHeight: 1.5 }}>
          Comprehensive record of matched trades, delivery escrow handoffs, and double-entry balance statements.
        </p>
      </div>

      <div className="history-header">
        <div className="segmented">
          <button
            type="button"
            className={activeTab === 'trades' ? 'active' : ''}
            onClick={() => setActiveTab('trades')}
          >
            Fills & Trades
          </button>
          <button
            type="button"
            className={activeTab === 'escrow' ? 'active' : ''}
            onClick={() => setActiveTab('escrow')}
          >
            Escrow
          </button>
          <button
            type="button"
            className={activeTab === 'statements' ? 'active' : ''}
            onClick={() => setActiveTab('statements')}
          >
            Audit Statements
          </button>
        </div>

        <div className="filter-group">
          <button
            type="button"
            className="ghost-button"
            onClick={refresh}
            title="Refresh history records"
          >
            ↻ Refresh
          </button>
          {activeTab === 'trades' && (
            <label className="filter-label">
              Side:
              <select
                value={sideFilter}
                onChange={(e) => setSideFilter(e.target.value)}
              >
                <option value="all">All Sides</option>
                <option value="buy">Buys Only</option>
                <option value="sell">Sells Only</option>
              </select>
            </label>
          )}
        </div>
      </div>

      {activeTab === 'escrow' ? (
        <section className="panel history-table-panel">
          <div className="panel-header">
            <h3>Escrow Handoffs</h3>
            <span className="panel-meta">
              {escrowItems.length} record{escrowItems.length === 1 ? '' : 's'}
            </span>
          </div>

          <div className="escrow-toolbar">
            <span className="muted">
              {escrowPublicKey
                ? `Seller key registered · ${escrowPublicKey.slice(0, 12)}…`
                : 'No seller signing key registered yet.'}
            </span>
            <button
              type="button"
              className="ghost-button"
              disabled={escrowLoading}
              onClick={async () => {
                setEscrowError(null)
                try {
                  await registerSigningKey()
                } catch (err) {
                  setEscrowError(err.message)
                }
              }}
            >
              {escrowPublicKey ? 'Rotate signing key' : 'Register signing key'}
            </button>
          </div>

          {escrowError && <div className="banner error">{escrowError}</div>}

          {escrowItems.length === 0 ? (
            <div className="empty-row">No escrow handoffs for this account.</div>
          ) : (
            <div className="table-responsive">
              <table className="data-table">
                <thead>
                  <tr>
                    <th>Trade</th>
                    <th>Market</th>
                    <th>Role</th>
                    <th>Qty</th>
                    <th>Escrow Value</th>
                    <th>State</th>
                    <th>Deadline</th>
                    <th>Action</th>
                  </tr>
                </thead>
                <tbody>
                  {escrowItems.map((item) => {
                    const pending = item.state === 'pending_delivery'
                    const expired = pending && now >= item.deadline_ms
                    const busy = escrowBusy === item.trade_id
                    const value = item.role === 'buyer' ? item.buyer_cash : item.seller_cash
                    return (
                      <tr key={`escrow-${item.trade_id}`}>
                        <td className="mono text-purple">#{item.trade_id}</td>
                        <td>
                          <span className="market-badge">{item.market_name}</span>
                        </td>
                        <td>
                          <span className={`side-tag ${item.role === 'buyer' ? 'buy' : 'sell'}`}>
                            {item.role.toUpperCase()}
                          </span>
                        </td>
                        <td className="mono">{formatQty(item.qty)}</td>
                        <td className="mono font-bold">{formatMoney(value)}</td>
                        <td>
                          <span className="kind-badge">
                            {item.state.replaceAll('_', ' ')}
                          </span>
                        </td>
                        <td className="mono muted">
                          {pending ? formatTime(item.deadline_ms) : '—'}
                        </td>
                        <td>
                          {pending && item.role === 'seller' && !expired && (
                            <button
                              type="button"
                              className="ghost-button"
                              disabled={busy || !escrowPublicKey}
                              onClick={() =>
                                runEscrowAction(item.trade_id, deliverEscrow)
                              }
                              title={
                                escrowPublicKey
                                  ? 'Sign the delivery receipt'
                                  : 'Register a signing key first'
                              }
                            >
                              {busy ? 'Signing…' : 'Mark delivered'}
                            </button>
                          )}
                          {pending && expired && (
                            <button
                              type="button"
                              className="ghost-button"
                              disabled={busy}
                              onClick={() =>
                                runEscrowAction(item.trade_id, refundEscrowAction)
                              }
                            >
                              {busy ? 'Refunding…' : 'Refund'}
                            </button>
                          )}
                          {pending && item.role === 'buyer' && !expired && (
                            <span className="muted">Waiting for seller</span>
                          )}
                          {!pending && <span className="muted">Closed</span>}
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          )}
        </section>
      ) : activeTab === 'trades' ? (
        <section className="panel history-table-panel">
          <div className="panel-header">
            <h3>Trade Executions</h3>
            <span className="panel-meta">{historyItems.length} record{historyItems.length === 1 ? '' : 's'}</span>
          </div>

          {historyItems.length === 0 ? (
            <div className="empty-row">No trade records found for this account.</div>
          ) : (
            <div className="table-responsive">
              <table className="data-table">
                <thead>
                  <tr>
                    <th>Time</th>
                    <th>Market</th>
                    <th>Side</th>
                    <th>Price</th>
                    <th>Qty</th>
                    <th>Fee</th>
                    <th>Net Amount</th>
                    <th>Status</th>
                  </tr>
                </thead>
                <tbody>
                  {historyItems.map((item, idx) => (
                    <tr key={`trade-${item.id ?? 'fill'}-${item.market_id ?? 0}-${idx}`}>
                      <td className="mono muted">{formatTime(item.ts)}</td>
                      <td>
                        <span className="market-badge">{item.event_name || `M#${item.market_id}`}</span>
                        {item.tier_name && <small className="tier-tag">{item.tier_name}</small>}
                      </td>
                      <td>
                        <span className={`side-tag ${item.side === 'buy' ? 'buy' : 'sell'}`}>
                          {item.side ? item.side.toUpperCase() : 'TRADE'}
                        </span>
                      </td>
                      <td className="mono">{formatMoney(item.price)}</td>
                      <td className="mono">{formatQty(item.qty)}</td>
                      <td className="mono text-muted">{formatMoney(item.fee)}</td>
                      <td className="mono font-bold">
                        {formatMoney(item.total_paise || (item.price * item.qty))}
                      </td>
                      <td>
                        <span className="status-pill settled">
                          {item.escrowed ? 'Escrow' : 'Settled'}
                        </span>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      ) : (
        <section className="panel history-table-panel">
          <div className="panel-header">
            <h3>Double-Entry Statements</h3>
            <span className="panel-meta">Immutable Ledger Log</span>
          </div>

          {statementItems.length === 0 ? (
            <div className="empty-row">No ledger statements recorded yet.</div>
          ) : (
            <div className="table-responsive">
              <table className="data-table">
                <thead>
                  <tr>
                    <th>Entry ID</th>
                    <th>Timestamp</th>
                    <th>Kind</th>
                    <th>Account</th>
                    <th>Asset</th>
                    <th>Amount</th>
                  </tr>
                </thead>
                <tbody>
                  {statementItems.map((st, idx) => (
                    <tr key={`stmt-${st.id ?? 'entry'}-${idx}`}>
                      <td className="mono text-purple">#{st.id}</td>
                      <td className="mono muted">{formatTime(st.ts)}</td>
                      <td>
                        <span className="kind-badge">{formatKind(st.kind)}</span>
                      </td>
                      <td className="mono text-strong">{formatAccount(st.account)}</td>
                      <td>{formatAsset(st.asset)}</td>
                      <td className={`mono font-bold ${st.amount >= 0 ? 'text-green' : 'text-red'}`}>
                        {st.amount >= 0 ? '+' : ''}
                        {st.asset === 'Cash' ? formatMoney(st.amount) : `${formatQty(st.amount)} passes`}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      )}
    </div>
  )
}
