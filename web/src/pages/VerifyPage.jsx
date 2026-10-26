import { useCallback, useEffect, useState } from 'react'
import { getMySolvency } from '../api'
import { formatMoney, formatQty } from '../format'
import {
  hashMerkleLeafWithAsset,
  hexToBytes,
  verifyMerklePath,
} from '../lib/hash'

const shortHash = (hash) => `${hash.slice(0, 10)}…${hash.slice(-8)}`

const assetName = (asset) =>
  asset.kind === 'pass' ? `Market #${asset.market_id} pass` : 'Cash'

const amountText = (asset, amount) =>
  asset.kind === 'cash' ? formatMoney(amount) : `${formatQty(amount)} passes`

async function verifyProof(proof) {
  const kind = proof.asset.kind === 'pass' ? 1 : 0
  const marketId = proof.asset.kind === 'pass' ? proof.asset.market_id : 0
  const leafHash = await hashMerkleLeafWithAsset(
    hexToBytes(proof.salt),
    BigInt(proof.user_id),
    kind,
    marketId,
    BigInt(proof.balance),
  )
  const path = proof.path.map((step) => ({
    siblingHash: hexToBytes(step.sibling_hash),
    siblingSum: BigInt(step.sibling_sum),
    siblingDirection: step.sibling_direction,
  }))

  return verifyMerklePath(
    leafHash,
    BigInt(proof.balance),
    path,
    hexToBytes(proof.root_hash),
    BigInt(proof.total_liabilities),
  )
}

async function fetchAndVerify() {
  const next = await getMySolvency()
  const checks = await Promise.all(
    next.proofs.map(async (proof) => ({
      proof,
      valid: await verifyProof(proof),
    })),
  )
  return { next, checks }
}

export function VerifyPage() {
  const [data, setData] = useState(null)
  const [checks, setChecks] = useState([])
  const [status, setStatus] = useState('loading')
  const [error, setError] = useState(null)

  const load = useCallback(async () => {
    setStatus('loading')
    setError(null)
    try {
      const result = await fetchAndVerify()
      setData(result.next)
      setChecks(result.checks)
      setStatus('ready')
    } catch (err) {
      setError(err.message)
      setStatus('error')
    }
  }, [])

  useEffect(() => {
    let cancelled = false
    fetchAndVerify()
      .then((result) => {
        if (cancelled) return
        setData(result.next)
        setChecks(result.checks)
        setStatus('ready')
      })
      .catch((err) => {
        if (cancelled) return
        setError(err.message)
        setStatus('error')
      })
    return () => {
      cancelled = true
    }
  }, [])

  const verified = checks.filter((check) => check.valid).length
  const allValid = checks.length > 0 && verified === checks.length

  return (
    <div className="verify-page">
      <div className="verify-heading">
        <div>
          <span className="eyebrow">Trust layer</span>
          <h2>Proof of solvency</h2>
        </div>
        <button type="button" className="ghost-button" onClick={load} disabled={status === 'loading'}>
          {status === 'loading' ? 'Verifying…' : '↻ Refresh & verify'}
        </button>
      </div>

      <p className="verify-intro">
        Each asset has a Merkle-sum tree whose root commits to total user liabilities. This page
        recomputes your leaf and walks its sibling path in your browser using the same hash format
        as the Rust core. It proves inclusion in the published liabilities, not that reserves are
        backed by real funds.
      </p>

      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}

      {data && (
        <>
          <div className="verify-grid">
            <div className="verify-stat">
              <span className="eyebrow">Snapshot</span>
              <strong>#{data.seq}</strong>
              <span>Core state version</span>
            </div>
            <div className="verify-stat">
              <span className="eyebrow">Asset trees</span>
              <strong>{data.roots.length}</strong>
              <span>One root per asset</span>
            </div>
            <div className="verify-stat">
              <span className="eyebrow">Your proofs</span>
              <strong>{checks.length}</strong>
              <span>Balance inclusion paths</span>
            </div>
            <div className={`verify-stat ${allValid ? 'good' : 'bad'}`}>
              <span className="eyebrow">Browser check</span>
              <strong>{verified}/{checks.length}</strong>
              <span>{allValid ? 'All proofs valid' : 'One or more proofs failed'}</span>
            </div>
          </div>

          <section className="panel verify-card">
            <div className="panel-header">
              <h3>Published roots</h3>
              <span className="panel-meta">Total liabilities per asset</span>
            </div>
            <div className="verify-table-wrap">
              <table className="verify-table">
                <thead>
                  <tr>
                    <th>Asset</th>
                    <th>Root hash</th>
                    <th>Total liabilities</th>
                    <th>Leaves</th>
                  </tr>
                </thead>
                <tbody>
                  {data.roots.map((root) => (
                    <tr key={`${root.asset.kind}-${root.asset.market_id ?? 0}`}>
                      <td>{assetName(root.asset)}</td>
                      <td className="mono verify-hash" title={root.root_hash}>
                        {shortHash(root.root_hash)}
                      </td>
                      <td className="mono">
                        {amountText(root.asset, root.total_liabilities)}
                      </td>
                      <td className="mono">{root.leaf_count}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </section>

          <section className="panel verify-card">
            <div className="panel-header">
              <h3>Your inclusion proofs</h3>
              <span className="panel-meta">Computed locally with Web Crypto</span>
            </div>
            {checks.length === 0 ? (
              <p className="verify-empty">No proofs were returned for this account.</p>
            ) : (
              <div className="verify-table-wrap">
                <table className="verify-table">
                  <thead>
                    <tr>
                      <th>Asset</th>
                      <th>Your balance</th>
                      <th>Path</th>
                      <th>Leaf</th>
                      <th>Status</th>
                    </tr>
                  </thead>
                  <tbody>
                    {checks.map(({ proof, valid }) => (
                      <tr key={`${proof.asset.kind}-${proof.asset.market_id ?? 0}`}>
                        <td>{assetName(proof.asset)}</td>
                        <td className="mono">{amountText(proof.asset, proof.balance)}</td>
                        <td className="mono">{proof.path.length} steps</td>
                        <td className="mono verify-hash" title={proof.salt}>
                          {shortHash(proof.salt)}
                        </td>
                        <td>
                          <span className={`verify-status ${valid ? 'good' : 'bad'}`}>
                            {valid ? '✓ valid' : '✕ invalid'}
                          </span>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </section>
        </>
      )}
    </div>
  )
}
