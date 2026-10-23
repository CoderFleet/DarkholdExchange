import { useState } from 'react'
import { formatMoney, formatQty } from '../format'

const PRESET_AMOUNTS = [500, 1000, 2500, 5000, 10000]

export function WalletView({ wallet, onTopup, onRefresh }) {
  const [customRupees, setCustomRupees] = useState('')
  const [topupSuccess, setTopupSuccess] = useState(null)
  const [topupError, setTopupError] = useState(null)
  const [loading, setLoading] = useState(false)

  const { cashAvail, cashLocked, escrowCash, passes } = wallet

  const totalCash = cashAvail + cashLocked + escrowCash

  const handleTopup = async (rupees) => {
    setLoading(true)
    setTopupError(null)
    setTopupSuccess(null)
    try {
      await onTopup(rupees)
      setTopupSuccess(`Successfully added ₹${rupees.toLocaleString('en-IN')}`)
      setCustomRupees('')
      setTimeout(() => setTopupSuccess(null), 4000)
    } catch (err) {
      setTopupError(err.message)
    } finally {
      setLoading(false)
    }
  }

  return (
    <div className="wallet-view">
      <div className="wallet-summary-grid">
        <div className="stat-card primary">
          <span className="eyebrow">Portfolio Value</span>
          <strong className="stat-value">{formatMoney(totalCash)}</strong>
          <span className="stat-sub">Double-entry ledger backed</span>
        </div>

        <div className="stat-card">
          <span className="eyebrow">Available Cash</span>
          <strong className="stat-value text-green">{formatMoney(cashAvail)}</strong>
          <span className="stat-sub">Spendable for new orders</span>
        </div>

        <div className="stat-card">
          <span className="eyebrow">Locked Cash</span>
          <strong className="stat-value text-amber">{formatMoney(cashLocked)}</strong>
          <span className="stat-sub">Collateral in resting bids</span>
        </div>

        <div className="stat-card">
          <span className="eyebrow">Escrow Balance</span>
          <strong className="stat-value text-purple">{formatMoney(escrowCash)}</strong>
          <span className="stat-sub">Protected for active handoffs</span>
        </div>
      </div>

      <div className="wallet-columns">
        {/* Top-up Card */}
        <section className="panel topup-panel">
          <div className="panel-header">
            <h3>Top Up Wallet</h3>
            <span className="panel-meta">Instant · Max ₹1,00,000</span>
          </div>

          <div className="preset-buttons">
            {PRESET_AMOUNTS.map((amt) => (
              <button
                key={amt}
                type="button"
                className="preset-button"
                disabled={loading}
                onClick={() => handleTopup(amt)}
              >
                + ₹{amt.toLocaleString('en-IN')}
              </button>
            ))}
          </div>

          <form
            className="custom-topup-form"
            onSubmit={(e) => {
              e.preventDefault()
              if (customRupees && Number(customRupees) > 0) {
                handleTopup(Number(customRupees))
              }
            }}
          >
            <label className="field">
              <span>Custom amount <em>Rupees (₹)</em></span>
              <input
                type="number"
                min="10"
                max="100000"
                step="10"
                placeholder="e.g. 1500"
                value={customRupees}
                onChange={(e) => setCustomRupees(e.target.value)}
                disabled={loading}
              />
            </label>

            <button
              type="submit"
              className="submit-button buy"
              disabled={loading || !customRupees || Number(customRupees) <= 0}
            >
              {loading ? 'Processing…' : `Top Up ${customRupees ? `₹${Number(customRupees).toLocaleString('en-IN')}` : ''}`}
            </button>
          </form>

          {topupSuccess && <div className="banner success">{topupSuccess}</div>}
          {topupError && <div className="banner error">{topupError}</div>}
        </section>

        {/* Passes Portfolio */}
        <section className="panel passes-panel">
          <div className="panel-header">
            <h3>Event Passes Owned</h3>
            <div style={{ display: 'flex', gap: '8px', alignItems: 'center' }}>
              <button
                type="button"
                className="ghost-button"
                onClick={onRefresh}
                title="Refresh balances"
              >
                ↻ Refresh
              </button>
              <span className="panel-meta">{passes.length} asset{passes.length === 1 ? '' : 's'}</span>
            </div>
          </div>

          {passes.length === 0 ? (
            <div className="empty-row">
              No event passes in wallet. Buy passes on the trading terminal.
            </div>
          ) : (
            <div className="passes-list">
              {passes.map((p, idx) => (
                <div className="pass-item" key={p.market_id ?? idx}>
                  <div className="pass-info">
                    <span className="pass-title">{p.event_name || `Market #${p.market_id}`}</span>
                    <span className="pass-tier badge">{p.tier_name || 'General Admission'}</span>
                  </div>
                  <div className="pass-numbers">
                    <div className="number-block">
                      <span className="label">Available</span>
                      <strong>{formatQty(p.avail)}</strong>
                    </div>
                    {p.locked > 0 && (
                      <div className="number-block locked">
                        <span className="label">Resting in Asks</span>
                        <strong>{formatQty(p.locked)}</strong>
                      </div>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </section>
      </div>
    </div>
  )
}
