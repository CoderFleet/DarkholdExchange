import { useEffect, useState } from 'react'
import { formatQty } from '../format'
import { MAX_ISSUE_QTY } from '../issuerApi'
import { qtyProblem } from '../issuerRules'
import { useIssuerStore } from '../stores/useIssuerStore'

function IssueForm() {
  const supply = useIssuerStore((state) => state.supply)
  const issue = useIssuerStore((state) => state.issue)
  const busy = useIssuerStore((state) => state.busy)
  const [username, setUsername] = useState('')
  const [marketId, setMarketId] = useState('')
  const [qty, setQty] = useState('')

  // Until the issuer picks one, the first market is used.
  const chosenMarket = Number(marketId || supply[0]?.market_id)
  const problem = qty === '' ? null : qtyProblem(qty)
  const ready = username.trim() !== '' && supply.length > 0 && qty !== '' && problem === null

  const handleSubmit = async (event) => {
    event.preventDefault()
    if (!ready) return
    if (await issue(username, chosenMarket, Number(qty.trim()))) {
      setQty('')
    }
  }

  return (
    <form className="panel admin-card" onSubmit={handleSubmit}>
      <h3>Credit passes</h3>
      <div className="admin-fields">
        <label className="field">
          <span>Username</span>
          <input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoComplete="off"
            maxLength={32}
            required
          />
        </label>
        <label className="field">
          <span>Market</span>
          <select value={marketId || supply[0]?.market_id || ''} onChange={(e) => setMarketId(e.target.value)}>
            {supply.map((row) => (
              <option key={row.market_id} value={row.market_id}>
                {row.name} (#{row.market_id})
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>Passes (1-{MAX_ISSUE_QTY})</span>
          <input
            value={qty}
            onChange={(e) => setQty(e.target.value)}
            inputMode="numeric"
            aria-invalid={problem ? 'true' : undefined}
            required
          />
        </label>
      </div>
      {problem && <p className="field-error">{problem}</p>}
      <button type="submit" className="auth-button admin-button" disabled={busy || !ready}>
        Credit passes
      </button>
    </form>
  )
}

function SupplyTable() {
  const supply = useIssuerStore((state) => state.supply)
  const status = useIssuerStore((state) => state.status)

  return (
    <section className="panel admin-card">
      <h3>Passes issued so far</h3>
      {status === 'loading' ? (
        <p className="admin-empty">Loading…</p>
      ) : supply.length === 0 ? (
        <p className="admin-empty">No markets yet. An admin has to publish a tier first.</p>
      ) : (
        <table className="admin-table">
          <thead>
            <tr>
              <th>Market</th>
              <th>Passes held by users</th>
            </tr>
          </thead>
          <tbody>
            {supply.map((row) => (
              <tr key={row.market_id}>
                <td>
                  {row.name} <span className="admin-venue">#{row.market_id}</span>
                </td>
                <td className="mono">{formatQty(row.passes_held)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  )
}

export function IssuerPage() {
  const error = useIssuerStore((state) => state.error)
  const notice = useIssuerStore((state) => state.notice)
  const load = useIssuerStore((state) => state.load)

  useEffect(() => {
    load()
  }, [load])

  return (
    <div className="admin-page">
      <h2>Issuer console</h2>
      <p className="admin-intro">
        Credit passes to a user by username. Passes only come into existence here. Pressing the
        button again after a network error is safe: the same request is never counted twice.
      </p>
      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}
      {notice && (
        <div className="banner success" role="status">
          {notice}
        </div>
      )}
      <IssueForm />
      <SupplyTable />
    </div>
  )
}