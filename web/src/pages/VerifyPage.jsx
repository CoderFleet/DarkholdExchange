import { useCallback, useEffect, useState } from 'react'
import {
  getBalanceProof,
  getSolvencyProof,
  getAuditLog,
  getAuditHead,
} from '../api'
import { formatMoney } from '../format'
import {
  hashMerkleLeaf,
  hashMerkleNode,
  hexToBytes,
  bytesToHex,
  verifyAuditChain,
  verifyMerklePath,
} from '../lib/hash'
import { useAuthStore } from '../stores/useAuthStore'

const shortHash = (hash) =>
  hash ? `${hash.slice(0, 10)}…${hash.slice(-8)}` : '—'

const AUDIT_KINDS = {
  1: 'Trade',
  2: 'Issue',
  3: 'EscrowReleased',
  4: 'EscrowRefunded',
  5: 'MarketPublished',
}

export function VerifyPage() {
  const user = useAuthStore((state) => state.user)
  const [activeTab, setActiveTab] = useState('balance') // 'balance' | 'audit'

  // Tab 1: Balance Proof state
  const [solvencyRoot, setSolvencyRoot] = useState(null)
  const [balanceProof, setBalanceProof] = useState(null)
  const [balanceStatus, setBalanceStatus] = useState('idle') // 'idle' | 'loading' | 'verified' | 'failed' | 'error'
  const [balanceError, setBalanceError] = useState(null)
  const [verificationSteps, setVerificationSteps] = useState([])
  const [nowSecs, setNowSecs] = useState(() => Math.floor(Date.now() / 1000))

  // Tab 2: Audit Log state
  const [auditRecords, setAuditRecords] = useState([])
  const [auditHead, setAuditHead] = useState(null)
  const [auditStatus, setAuditStatus] = useState('idle') // 'idle' | 'loading' | 'verified' | 'broken' | 'error'
  const [brokenSeq, setBrokenSeq] = useState(null)
  const [auditError, setAuditError] = useState(null)

  // Load published solvency root
  const loadSolvency = useCallback(async () => {
    try {
      const root = await getSolvencyProof()
      setSolvencyRoot(root)
    } catch (err) {
      console.error('Failed to load solvency root', err)
    }
  }, [])

  // Load user balance proof
  const loadBalanceProof = useCallback(async () => {
    if (!user) {
      setBalanceProof(null)
      return
    }
    setBalanceError(null)
    try {
      const proof = await getBalanceProof()
      setBalanceProof(proof)
    } catch (err) {
      setBalanceProof(null)
      if (err.status !== 404) {
        setBalanceError(err.message)
      }
    }
  }, [user])

  // Load audit log & head
  const loadAuditData = useCallback(async () => {
    setAuditStatus('loading')
    setAuditError(null)
    setBrokenSeq(null)
    try {
      const [records, head] = await Promise.all([
        getAuditLog({ limit: 200 }),
        getAuditHead(),
      ])
      setAuditRecords(records || [])
      setAuditHead(head)
      setAuditStatus('idle')
    } catch (err) {
      setAuditError(err.message)
      setAuditStatus('error')
    }
  }, [])

  useEffect(() => {
    const timer = setInterval(() => {
      setNowSecs(Math.floor(Date.now() / 1000))
    }, 10000)

    const initial = setTimeout(() => {
      loadSolvency()
      loadBalanceProof()
      loadAuditData()
    }, 0)

    return () => {
      clearTimeout(initial)
      clearInterval(timer)
    }
  }, [loadSolvency, loadBalanceProof, loadAuditData])

  // Browser-side Level-by-level Merkle Path Verification
  const handleVerifyBalance = async () => {
    if (!balanceProof) return
    setBalanceStatus('loading')
    setVerificationSteps([])

    try {
      const saltBytes = hexToBytes(balanceProof.leaf.salt_hex)
      const userId = BigInt(balanceProof.leaf.user_id)
      const balancePaise = BigInt(balanceProof.leaf.balance_paise)

      // Step 0: Leaf hash
      const leafHashBytes = await hashMerkleLeaf(saltBytes, userId, balancePaise)
      let currentHash = leafHashBytes
      let currentSum = balancePaise

      const steps = []
      steps.push({
        level: 0,
        type: 'Leaf',
        hash: bytesToHex(currentHash),
        sum: currentSum.toString(),
        valid: true,
      })

      // Walk path level by level
      for (let i = 0; i < balanceProof.path.length; i++) {
        const step = balanceProof.path[i]
        const siblingHashBytes = hexToBytes(step.hash_hex)
        const siblingSum = BigInt(step.sum_paise)

        let parentNode
        if (step.is_left) {
          parentNode = await hashMerkleNode(
            siblingHashBytes,
            siblingSum,
            currentHash,
            currentSum
          )
        } else {
          parentNode = await hashMerkleNode(
            currentHash,
            currentSum,
            siblingHashBytes,
            siblingSum
          )
        }

        currentHash = parentNode.hash
        currentSum = parentNode.sum

        steps.push({
          level: i + 1,
          type: `Parent L${i + 1}`,
          siblingHash: step.hash_hex,
          siblingSum: step.sum_paise,
          direction: step.is_left ? 'Left' : 'Right',
          resultingHash: bytesToHex(currentHash),
          resultingSum: currentSum.toString(),
          valid: true,
        })
      }

      setVerificationSteps(steps)

      const expectedRootHash = hexToBytes(balanceProof.root_hash_hex)
      const expectedTotal = BigInt(balanceProof.total_paise)

      const pathSteps = balanceProof.path.map((s) => ({
        siblingHash: hexToBytes(s.hash_hex),
        siblingSum: BigInt(s.sum_paise),
        siblingDirection: s.is_left ? 'Left' : 'Right',
      }))

      const isValid = await verifyMerklePath(
        leafHashBytes,
        balancePaise,
        pathSteps,
        expectedRootHash,
        expectedTotal
      )

      setBalanceStatus(isValid ? 'verified' : 'failed')
    } catch (err) {
      console.error(err)
      setBalanceStatus('failed')
      setBalanceError(err.message)
    }
  }

  // Browser-side Audit Chain Verification
  const handleVerifyChain = async () => {
    if (!auditRecords || auditRecords.length === 0) return
    setAuditStatus('loading')
    setBrokenSeq(null)

    try {
      const recordsToVerify = auditRecords.map((r) => ({
        seq: BigInt(r.seq),
        tsMicros: BigInt(r.ts_micros),
        prev: hexToBytes(r.prev_hash_hex),
        kind: r.kind,
        market: r.market_id,
        qty: r.qty,
        price: BigInt(r.price_paise),
        hash: hexToBytes(r.hash_hex),
      }))

      // Check continuity and hashes item by item to locate first broken row
      let brokenFound = null
      for (let i = 0; i < recordsToVerify.length; i++) {
        const item = recordsToVerify[i]
        if (i > 0) {
          if (auditRecords[i].prev_hash_hex !== auditRecords[i - 1].hash_hex) {
            brokenFound = item.seq
            break
          }
        }
        const singleValid = await verifyAuditChain([item])
        if (!singleValid) {
          brokenFound = item.seq
          break
        }
      }

      if (brokenFound !== null) {
        setBrokenSeq(brokenFound)
        setAuditStatus('broken')
      } else {
        const overallValid = await verifyAuditChain(recordsToVerify)
        if (overallValid) {
          setAuditStatus('verified')
        } else {
          setAuditStatus('broken')
          setBrokenSeq(auditRecords[0]?.seq ?? null)
        }
      }
    } catch (err) {
      console.error(err)
      setAuditStatus('error')
      setAuditError(err.message)
    }
  }

  const rootAgeMinutes = solvencyRoot?.published_at && nowSecs > 0
    ? Math.max(0, Math.floor((nowSecs - solvencyRoot.published_at) / 60))
    : 0

  return (
    <div className="verify-page">
      <div className="verify-heading">
        <div>
          <span className="eyebrow">Trust & Cryptography</span>
          <h2>Cryptographic Proofs & Audit Chain</h2>
        </div>
      </div>

      <p className="verify-intro">
        Darkhold Exchange provides mathematically verifiable solvency proofs and an immutable public
        audit chain. Every user can independently verify their balance inclusion and the integrity
        of public exchange operations entirely inside their browser using the TypeScript twin of our
        Rust cryptographic core.
      </p>

      {/* Tabs */}
      <div className="verify-tabs" style={{ display: 'flex', gap: '0.75rem', marginBottom: '1.5rem', borderBottom: '1px solid rgba(255, 255, 255, 0.08)' }}>
        <button
          type="button"
          onClick={() => setActiveTab('balance')}
          style={{
            padding: '0.75rem 1.25rem',
            background: activeTab === 'balance' ? 'rgba(179, 136, 255, 0.12)' : 'none',
            border: 'none',
            borderBottom: activeTab === 'balance' ? '2px solid #b388ff' : '2px solid transparent',
            color: activeTab === 'balance' ? '#ffffff' : '#94a3b8',
            borderRadius: '6px 6px 0 0',
            cursor: 'pointer',
            fontWeight: 600,
            fontSize: '14px',
            transition: 'all 0.15s ease',
          }}
        >
          My Balance Proof
        </button>
        <button
          type="button"
          onClick={() => setActiveTab('audit')}
          style={{
            padding: '0.75rem 1.25rem',
            background: activeTab === 'audit' ? 'rgba(179, 136, 255, 0.12)' : 'none',
            border: 'none',
            borderBottom: activeTab === 'audit' ? '2px solid #b388ff' : '2px solid transparent',
            color: activeTab === 'audit' ? '#ffffff' : '#94a3b8',
            borderRadius: '6px 6px 0 0',
            cursor: 'pointer',
            fontWeight: 600,
            fontSize: '14px',
            transition: 'all 0.15s ease',
          }}
        >
          Public Audit Log
        </button>
      </div>

      {/* Tab 1: Balance Proof */}
      {activeTab === 'balance' && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: '1.75rem' }}>
          <div className="banner" style={{ background: 'rgba(179, 136, 255, 0.06)', border: '1px solid rgba(179, 136, 255, 0.25)', padding: '1rem 1.25rem', borderRadius: '10px', color: '#cbd5e1', fontSize: '13px', lineHeight: 1.6 }}>
            🛡️ <strong>Cryptographic Guarantee:</strong> Merkle-sum tree roots commit to total user liabilities. Exchange reserves are simulated for this demonstration. The tree is rebuilt every 30s at known snapshot sequences.
          </div>

          <div className="verify-grid">
            <div className="verify-stat">
              <span className="eyebrow">Published Root</span>
              <strong className="mono" title={solvencyRoot?.root_hash_hex || ''}>
                {shortHash(solvencyRoot?.root_hash_hex)}
              </strong>
              <span>Seq #{solvencyRoot?.seq ?? 0}</span>
            </div>
            <div className="verify-stat">
              <span className="eyebrow">Total Liabilities</span>
              <strong style={{ color: '#4ade80' }}>{formatMoney(solvencyRoot?.total_paise ?? 0)}</strong>
              <span>Across {solvencyRoot?.leaf_count ?? 0} accounts</span>
            </div>
            <div className="verify-stat">
              <span className="eyebrow">Root Age</span>
              <strong>{rootAgeMinutes} min ago</strong>
              <span>Rebuilt every 30s</span>
            </div>
            <div className={`verify-stat ${balanceStatus === 'verified' ? 'good' : balanceStatus === 'failed' ? 'bad' : ''}`}>
              <span className="eyebrow">Verification Status</span>
              <strong>
                {balanceStatus === 'loading'
                  ? 'Verifying…'
                  : balanceStatus === 'verified'
                  ? '✓ Verified'
                  : balanceStatus === 'failed'
                  ? '✕ Failed'
                  : 'Ready'}
              </strong>
              <span>Browser Web Crypto</span>
            </div>
          </div>

          {balanceError && (
            <div className="banner error" role="alert" style={{ marginBottom: '1.5rem' }}>
              {balanceError}
            </div>
          )}

          {!user ? (
            <div className="panel verify-card" style={{ padding: '3rem 2rem', textAlign: 'center', background: 'rgba(255, 255, 255, 0.02)' }}>
              <p style={{ color: '#94a3b8', fontSize: '14px' }}>Please log in to inspect and verify your personal balance inclusion proof.</p>
            </div>
          ) : !balanceProof ? (
            <div className="panel verify-card" style={{ padding: '3rem 2rem', textAlign: 'center', background: 'rgba(255, 255, 255, 0.02)' }}>
              <p style={{ color: '#94a3b8', fontSize: '14px' }}>No balance proof record found for your account yet. Try depositing funds or waiting for the next tree update.</p>
              <button type="button" className="ghost-button" onClick={loadBalanceProof} style={{ marginTop: '1rem' }}>
                ↻ Refresh Proof
              </button>
            </div>
          ) : (
            <>
              <section className="panel verify-card" style={{ border: '1px solid rgba(255, 255, 255, 0.08)', borderRadius: '12px' }}>
                <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '14px', gap: '12px', flexWrap: 'wrap' }}>
                  <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start', textAlign: 'left', gap: '3px' }}>
                    <h3 style={{ fontSize: '17px', fontWeight: 600, margin: 0, color: 'var(--text-strong)' }}>Your Balance Leaf & Salt</h3>
                    <span style={{ fontSize: '11px', color: '#94a3b8', lineHeight: 1.4 }}>
                      Only you receive your personal 16-byte random salt
                    </span>
                  </div>
                  <button
                    type="button"
                    className="verify-action-btn"
                    onClick={handleVerifyBalance}
                    disabled={balanceStatus === 'loading'}
                  >
                    {balanceStatus === 'loading' ? (
                      <>
                        <span className="verify-spinner" />
                        Verifying Path…
                      </>
                    ) : (
                      <>
                        <span style={{ opacity: 0.8 }}>⚡</span>
                        Verify my balance
                      </>
                    )}
                  </button>
                </div>

                <div className="verify-table-wrap">
                  <table className="verify-table">
                    <thead>
                      <tr>
                        <th>User ID</th>
                        <th>Your Balance</th>
                        <th>Stable Salt (16-byte Hex)</th>
                        <th>Path Length</th>
                      </tr>
                    </thead>
                    <tbody>
                      <tr>
                        <td className="mono">{balanceProof.leaf.user_id}</td>
                        <td className="mono" style={{ color: '#38bdf8', fontWeight: 700, fontSize: '15px' }}>
                          {formatMoney(balanceProof.leaf.balance_paise)}
                        </td>
                        <td className="mono" title={balanceProof.leaf.salt_hex} style={{ color: '#e2e8f0' }}>
                          {balanceProof.leaf.salt_hex}
                        </td>
                        <td className="mono">{balanceProof.path.length} levels</td>
                      </tr>
                    </tbody>
                  </table>
                </div>
              </section>

              {/* Step-by-step level verification */}
              {verificationSteps.length > 0 && (
                <section className="panel verify-card" style={{ border: '1px solid rgba(255, 255, 255, 0.08)', borderRadius: '12px' }}>
                  <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start', textAlign: 'left', gap: '3px', marginBottom: '14px' }}>
                    <h3 style={{ fontSize: '15px', fontWeight: 600, margin: 0, color: 'var(--text-strong)' }}>Step-by-Step Level Walk</h3>
                    <span style={{ fontSize: '11px', color: '#94a3b8', lineHeight: 1.4 }}>
                      Browser recalculation from leaf hash up to published root
                    </span>
                  </div>
                  <div className="verify-table-wrap">
                    <table className="verify-table">
                      <thead>
                        <tr>
                          <th>Level</th>
                          <th>Type</th>
                          <th>Sibling Hash</th>
                          <th>Sibling Sum</th>
                          <th>Direction</th>
                          <th>Resulting Hash</th>
                          <th>Cumulative Sum</th>
                        </tr>
                      </thead>
                      <tbody>
                        {verificationSteps.map((st, idx) => (
                          <tr key={idx}>
                            <td className="mono">
                              <span style={{ padding: '2px 8px', borderRadius: '4px', background: 'rgba(255, 255, 255, 0.05)' }}>
                                L{st.level}
                              </span>
                            </td>
                            <td style={{ fontWeight: 500 }}>{st.type}</td>
                            <td className="mono" title={st.siblingHash || ''}>
                              {shortHash(st.siblingHash)}
                            </td>
                            <td className="mono" style={{ color: '#cbd5e1' }}>
                              {st.siblingSum != null ? formatMoney(st.siblingSum) : '—'}
                            </td>
                            <td>
                              <span style={{ color: st.direction === 'Left' ? '#38bdf8' : '#a78bfa' }}>
                                {st.direction || 'Self'}
                              </span>
                            </td>
                            <td className="mono" title={st.resultingHash || st.hash} style={{ color: '#38bdf8' }}>
                              {shortHash(st.resultingHash || st.hash)}
                            </td>
                            <td className="mono" style={{ fontWeight: 600 }}>
                              {formatMoney(st.resultingSum || st.sum)}
                            </td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  </div>
                </section>
              )}
            </>
          )}
        </div>
      )}

      {/* Tab 2: Audit Log */}
      {activeTab === 'audit' && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: '2rem' }}>
          <div className="verify-grid">
            <div className="verify-stat">
              <span className="eyebrow">Head Sequence</span>
              <strong>#{auditHead?.seq ?? (auditRecords[auditRecords.length - 1]?.seq ?? 0)}</strong>
              <span>Continuous record counter</span>
            </div>
            <div className="verify-stat">
              <span className="eyebrow">Head Hash</span>
              <strong className="mono" title={auditHead?.hash_hex || ''}>
                {shortHash(auditHead?.hash_hex || auditRecords[auditRecords.length - 1]?.hash_hex)}
              </strong>
              <span>Current audit tip</span>
            </div>
            <div className="verify-stat">
              <span className="eyebrow">Loaded Records</span>
              <strong>{auditRecords.length}</strong>
              <span>Public events (no user IDs)</span>
            </div>
            <div className={`verify-stat ${auditStatus === 'verified' ? 'good' : auditStatus === 'broken' ? 'bad' : ''}`}>
              <span className="eyebrow">Chain Verification</span>
              <strong>
                {auditStatus === 'loading'
                  ? 'Checking…'
                  : auditStatus === 'verified'
                  ? '✓ Continuous & Valid'
                  : auditStatus === 'broken'
                  ? `✕ Broken at #${brokenSeq}`
                  : 'Ready'}
              </strong>
              <span>Recomputed in Browser</span>
            </div>
          </div>

          {auditError && (
            <div className="banner error" role="alert" style={{ marginBottom: '1.5rem' }}>
              {auditError}
            </div>
          )}

          <section className="panel verify-card" style={{ border: '1px solid rgba(255, 255, 255, 0.08)', borderRadius: '12px' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '14px', gap: '12px', flexWrap: 'wrap' }}>
              <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'flex-start', textAlign: 'left', gap: '3px' }}>
                <h3 style={{ fontSize: '17px', fontWeight: 600, margin: 0, color: 'var(--text-strong)' }}>Public Audit Records</h3>
                <span style={{ fontSize: '11px', color: '#94a3b8', lineHeight: 1.4 }}>
                  DKX-AUD-v1 continuous SHA-256 hash chain without user identifiers
                </span>
              </div>
              <div style={{ display: 'flex', gap: '0.6rem', alignItems: 'center' }}>
                <button
                  type="button"
                  className="ghost-button"
                  onClick={loadAuditData}
                  disabled={auditStatus === 'loading'}
                  style={{ fontSize: '12px', padding: '6px 12px' }}
                >
                  ↻ Refresh Log
                </button>
                <button
                  type="button"
                  className="verify-action-btn"
                  onClick={handleVerifyChain}
                  disabled={auditStatus === 'loading' || auditRecords.length === 0}
                >
                  {auditStatus === 'loading' ? (
                    <>
                      <span className="verify-spinner" />
                      Recomputing Hashes…
                    </>
                  ) : (
                    <>
                      <span style={{ opacity: 0.8 }}>⛓️</span>
                      Verify chain
                    </>
                  )}
                </button>
              </div>
            </div>

            {auditRecords.length === 0 ? (
              <p className="verify-empty" style={{ padding: '3rem 2rem', textAlign: 'center', color: '#94a3b8' }}>
                No audit records generated yet. Trade or issue operations will create audit entries.
              </p>
            ) : (
              <div className="verify-table-wrap">
                <table className="verify-table">
                  <thead>
                    <tr>
                      <th>Seq</th>
                      <th>Time (µs)</th>
                      <th>Kind</th>
                      <th>Market</th>
                      <th>Quantity</th>
                      <th>Price</th>
                      <th>Prev Hash</th>
                      <th>Hash</th>
                      <th>Status</th>
                    </tr>
                  </thead>
                  <tbody>
                    {auditRecords.map((rec) => {
                      const isBrokenRow = brokenSeq != null && rec.seq === brokenSeq
                      return (
                        <tr
                          key={rec.seq}
                          style={
                            isBrokenRow
                              ? { backgroundColor: 'rgba(239, 68, 68, 0.2)', border: '1px solid #ef4444' }
                              : {}
                          }
                        >
                          <td className="mono">#{rec.seq}</td>
                          <td className="mono text-muted">{rec.ts_micros}</td>
                          <td>
                            <span className="mono" style={{ color: '#a78bfa', fontWeight: 600 }}>
                              {AUDIT_KINDS[rec.kind] || `Kind ${rec.kind}`}
                            </span>
                          </td>
                          <td className="mono">{rec.market_id === 0 ? '—' : `#${rec.market_id}`}</td>
                          <td className="mono">{rec.qty}</td>
                          <td className="mono">{formatMoney(rec.price_paise)}</td>
                          <td className="mono verify-hash" title={rec.prev_hash_hex}>
                            {shortHash(rec.prev_hash_hex)}
                          </td>
                          <td className="mono verify-hash" title={rec.hash_hex} style={{ color: '#38bdf8' }}>
                            {shortHash(rec.hash_hex)}
                          </td>
                          <td>
                            {isBrokenRow ? (
                              <span style={{ color: '#ef4444', fontWeight: 700 }}>✕ BROKEN</span>
                            ) : auditStatus === 'verified' ? (
                              <span style={{ color: '#22c55e', fontWeight: 600 }}>✓ OK</span>
                            ) : (
                              <span style={{ color: '#94a3b8' }}>—</span>
                            )}
                          </td>
                        </tr>
                      )
                    })}
                  </tbody>
                </table>
              </div>
            )}
          </section>
        </div>
      )}
    </div>
  )
}
