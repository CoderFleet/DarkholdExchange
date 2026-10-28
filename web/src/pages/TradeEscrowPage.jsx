import { useEffect, useState, useCallback } from 'react'
import {
  acceptEscrow,
  getEscrowById,
  rejectEscrow,
  verifyEscrowReceipt,
} from '../api'
import { formatMoney } from '../format'
import { useAuthStore } from '../stores/useAuthStore'
import { useWalletStore } from '../stores/useWalletStore'

const ESCROW_STEPS = ['Created', 'FundsLocked', 'Delivered', 'Released']

export function TradeEscrowPage({ tradeId }) {
  const authUser = useAuthStore((state) => state.user)
  const walletUserId = useWalletStore((state) => state.userId)
  const setWalletUserId = useWalletStore((state) => state.setUserId)
  const currentUserId = authUser?.user_id ?? authUser?.id ?? walletUserId ?? 1

  const [escrow, setEscrow] = useState(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)
  const [remainingSecs, setRemainingSecs] = useState(null)

  // Report modal
  const [showRejectModal, setShowRejectModal] = useState(false)
  const [rejectReason, setRejectReason] = useState('pass_invalid')
  const [rejectNote, setRejectNote] = useState('')
  const [actionBusy, setActionBusy] = useState(false)

  // Receipt verification
  const [verifying, setVerifying] = useState(false)
  const [verificationResult, setVerificationResult] = useState(null)

  const fetchEscrow = useCallback(async () => {
    if (!tradeId) return
    setLoading(true)
    setError(null)
    try {
      const data = await getEscrowById(tradeId, currentUserId)
      setEscrow(data)
    } catch (err) {
      setError(err.message || 'Escrow details not found')
    } finally {
      setLoading(false)
    }
  }, [tradeId, currentUserId])
  
  useEffect(() => {
    const timer = setTimeout(() => {
      fetchEscrow()
    }, 0)
    return () => clearTimeout(timer)
  }, [fetchEscrow])

// Countdown timer for inspection window
useEffect(() => {
  if (!escrow?.deadline || escrow.state !== 'Delivered') {
    return
  }

  const updateTimer = () => {
    const nowMicros = Date.now() * 1000
    const diffMicros = Number(escrow.deadline) - nowMicros
    const secs = Math.max(0, Math.floor(diffMicros / 1_000_000))
    setRemainingSecs(secs)
  }

  updateTimer()
  const interval = setInterval(updateTimer, 1000)

  return () => {
    clearInterval(interval)
    setRemainingSecs(null)
  }
}, [escrow?.deadline, escrow?.state])

  const isBuyer = escrow ? Number(escrow.buyer_id) === Number(currentUserId) : false
  const isSeller = escrow ? Number(escrow.seller_id) === Number(currentUserId) : false

  const handleAccept = async () => {
    if (!escrow) return
    setActionBusy(true)
    try {
      await acceptEscrow(escrow.id, currentUserId)
      await fetchEscrow()
    } catch (err) {
      alert(err.message || 'Failed to accept escrow')
    } finally {
      setActionBusy(false)
    }
  }

  const handleRejectSubmit = async (e) => {
    e.preventDefault()
    if (!escrow) return
    setActionBusy(true)
    try {
      await rejectEscrow(escrow.id, rejectReason, rejectNote, currentUserId)
      setShowRejectModal(false)
      await fetchEscrow()
    } catch (err) {
      alert(err.message || 'Failed to report issue')
    } finally {
      setActionBusy(false)
    }
  }

  const handleVerifyReceipt = async () => {
    if (!escrow) return
    setVerifying(true)
    setVerificationResult(null)
    try {
      const res = await verifyEscrowReceipt(escrow.id)
      setVerificationResult(res.valid ? 'valid' : 'invalid')
    } catch {
      setVerificationResult('invalid')
    } finally {
      setVerifying(false)
    }
  }

  const formatCountdown = (secs) => {
    if (secs == null) return ''
    const m = Math.floor(secs / 60)
    const s = secs % 60
    return `${m}:${s < 10 ? '0' : ''}${s}`
  }

  if (loading) {
    return <div style={{ padding: '2rem', textAlign: 'center' }}>Loading escrow trade #{tradeId}…</div>
  }

  if (error || !escrow) {
    return (
      <div style={{ padding: '2rem', maxWidth: '800px', margin: '0 auto' }}>
        <div className="banner error" style={{ background: '#7f1d1d', color: '#fca5a5', padding: '1rem', borderRadius: '6px' }}>
          {error || 'No escrow record found.'}
        </div>
      </div>
    )
  }

  const isDisputed = escrow.state === 'Disputed'
  const isReleased = escrow.state === 'Released'
  const isRefunded = escrow.state === 'Refunded'
  const isDelivered = escrow.state === 'Delivered'

  return (
    <div className="trade-escrow-page" style={{ padding: '2rem', maxWidth: '850px', margin: '0 auto' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', marginBottom: '1.5rem' }}>
        <div>
          <span className="eyebrow">Escrow Trade #{escrow.trade_id}</span>
          <h2>Escrow Inspection & Settlement</h2>
        </div>
        {!authUser && (
          <div style={{ textAlign: 'right' }}>
            <label style={{ fontSize: '0.8rem', color: '#9ca3af', display: 'block', marginBottom: '0.25rem' }}>
              Simulate View As Trader:
            </label>
            <select
              value={currentUserId}
              onChange={(e) => setWalletUserId(Number(e.target.value))}
              style={{
                background: '#1f2937',
                border: '1px solid #4b5563',
                color: '#fff',
                padding: '0.3rem 0.6rem',
                borderRadius: '4px',
                fontSize: '0.85rem',
              }}
            >
              {[1, 2, 3, 4, 5, 6, escrow.buyer_id, escrow.seller_id]
                .filter((v, i, a) => a.indexOf(v) === i)
                .map((id) => (
                  <option key={id} value={id}>
                    Trader #{id} {id === escrow.buyer_id ? '(Buyer)' : id === escrow.seller_id ? '(Seller)' : ''}
                  </option>
                ))}
            </select>
          </div>
        )}
      </div>

      {/* Disputed or Resolved Banners */}
      {isDisputed && (
        <div
          className="banner"
          style={{
            background: '#78350f',
            border: '1px solid #f59e0b',
            color: '#fef3c7',
            padding: '1rem',
            borderRadius: '6px',
            marginBottom: '1.5rem',
          }}
        >
          <strong>⚠️ Trade in Dispute:</strong> The buyer reported an issue. An admin is reviewing the dispute. Funds and passes remain securely held in escrow until an admin ruling is posted.
        </div>
      )}

      {isReleased && (
        <div
          className="banner"
          style={{
            background: '#064e3b',
            border: '1px solid #10b981',
            color: '#d1fae5',
            padding: '1rem',
            borderRadius: '6px',
            marginBottom: '1.5rem',
          }}
        >
          <strong>✓ Escrow Completed:</strong> Funds have been released to the seller and passes are now available in the buyer's wallet.
        </div>
      )}

      {isRefunded && (
        <div
          className="banner"
          style={{
            background: '#1e3a8a',
            border: '1px solid #3b82f6',
            color: '#dbeafe',
            padding: '1rem',
            borderRadius: '6px',
            marginBottom: '1.5rem',
          }}
        >
          <strong>↩ Escrow Refunded:</strong> Ruling complete. Full payment refunded to the buyer; pass returned to the seller.
        </div>
      )}

      {/* Stepper */}
      <div className="panel" style={{ padding: '1.5rem', marginBottom: '1.5rem', background: '#111827', borderRadius: '8px' }}>
        <h4 style={{ margin: '0 0 1rem 0' }}>Escrow Lifecycle</h4>
        <div style={{ display: 'flex', justifyContent: 'space-between', position: 'relative' }}>
          {ESCROW_STEPS.map((step, idx) => {
            let active = false
            let completed = false
            if (isReleased) {
              completed = true
            } else if (isDisputed || isRefunded) {
              if (idx <= 2) completed = true
            } else if (isDelivered) {
              if (idx < 2) completed = true
              if (idx === 2) active = true
            }

            return (
              <div key={step} style={{ textAlign: 'center', flex: 1, position: 'relative' }}>
                <div
                  style={{
                    width: '32px',
                    height: '32px',
                    borderRadius: '50%',
                    background: completed ? '#10b981' : active ? '#f59e0b' : '#374151',
                    color: '#fff',
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    margin: '0 auto 0.5rem auto',
                    fontWeight: 'bold',
                    fontSize: '0.85rem',
                  }}
                >
                  {completed ? '✓' : idx + 1}
                </div>
                <span style={{ fontSize: '0.8rem', color: active ? '#f59e0b' : completed ? '#10b981' : '#9ca3af' }}>
                  {step}
                </span>
              </div>
            )
          })}
        </div>
      </div>

      {/* Trade Info & Countdown */}
      <div className="panel" style={{ padding: '1.5rem', marginBottom: '1.5rem', background: '#111827', borderRadius: '8px' }}>
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(180px, 1fr))', gap: '1rem' }}>
          <div>
            <span style={{ color: '#9ca3af', fontSize: '0.75rem' }}>Quantity & Market</span>
            <p style={{ margin: '0.25rem 0', fontWeight: 'bold' }}>{escrow.qty} Pass (Market #{escrow.market_id})</p>
          </div>
          <div>
            <span style={{ color: '#9ca3af', fontSize: '0.75rem' }}>Total Locked Value</span>
            <p style={{ margin: '0.25rem 0', fontWeight: 'bold' }}>{formatMoney(escrow.price * escrow.qty)}</p>
          </div>
          <div>
            <span style={{ color: '#9ca3af', fontSize: '0.75rem' }}>Platform Fee</span>
            <p style={{ margin: '0.25rem 0', fontWeight: 'bold' }}>{formatMoney(escrow.fee)}</p>
          </div>
          <div>
            <span style={{ color: '#9ca3af', fontSize: '0.75rem' }}>Your Role</span>
            <p style={{ margin: '0.25rem 0', fontWeight: 'bold', color: '#60a5fa' }}>
              {isBuyer ? 'Buyer (Pass Holder)' : isSeller ? 'Seller (Funds Awaiting)' : 'Observer'}
            </p>
          </div>
        </div>

        {/* Deadline Countdown */}
        {isDelivered && remainingSecs !== null && (
          <div
            style={{
              marginTop: '1.5rem',
              padding: '1rem',
              background: '#1f2937',
              borderRadius: '6px',
              display: 'flex',
              justifyContent: 'space-between',
              alignItems: 'center',
            }}
          >
            <div>
              <strong style={{ display: 'block' }}>Inspection Window Countdown</strong>
              <span style={{ fontSize: '0.8rem', color: '#9ca3af' }}>
                Auto-releases to seller when timer expires if no problem is reported.
              </span>
            </div>
            <div className="mono" style={{ fontSize: '1.5rem', color: '#fbbf24', fontWeight: 'bold' }}>
              {formatCountdown(remainingSecs)}
            </div>
          </div>
        )}
      </div>

      {/* Buyer Action Controls */}
      {isBuyer && isDelivered && (
        <div className="panel" style={{ padding: '1.5rem', marginBottom: '1.5rem', background: '#111827', borderRadius: '8px' }}>
          <h4 style={{ margin: '0 0 1rem 0' }}>Buyer Inspection Actions</h4>
          <p style={{ fontSize: '0.875rem', color: '#9ca3af', marginBottom: '1rem' }}>
            Inspect your pass. Once you click Accept, payment is permanently released to the seller. If the pass is invalid or fraudulent, report a problem before the countdown ends.
          </p>
          <div style={{ display: 'flex', gap: '1rem' }}>
            <button
              type="button"
              className="auth-button"
              style={{ background: '#10b981', padding: '0.6rem 1.5rem' }}
              onClick={handleAccept}
              disabled={actionBusy}
            >
              {actionBusy ? 'Processing…' : '✓ Accept & Release Payment'}
            </button>
            <button
              type="button"
              className="auth-button"
              style={{ background: '#dc2626', padding: '0.6rem 1.5rem' }}
              onClick={() => setShowRejectModal(true)}
              disabled={actionBusy}
            >
              Report a Problem
            </button>
          </div>
        </div>
      )}

      {/* Seller View Notice */}
      {isSeller && isDelivered && (
        <div className="panel" style={{ padding: '1.5rem', marginBottom: '1.5rem', background: '#111827', borderRadius: '8px' }}>
          <h4 style={{ margin: '0 0 0.5rem 0' }}>Seller Status</h4>
          <p style={{ margin: 0, fontSize: '0.875rem', color: '#9ca3af' }}>
            The buyer is currently in their inspection window. Payment is held in secure platform escrow and will release upon buyer confirmation or countdown expiration.
          </p>
        </div>
      )}

      {/* Cryptographic Receipt Card */}
      <div className="panel" style={{ padding: '1.5rem', background: '#111827', borderRadius: '8px' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '1rem' }}>
          <div>
            <h4 style={{ margin: 0 }}>Ed25519 Signed Trade Receipt</h4>
            <span style={{ fontSize: '0.8rem', color: '#9ca3af' }}>Format: DKX-RCPT-v1</span>
          </div>
          <button
            type="button"
            className="ghost-button"
            onClick={handleVerifyReceipt}
            disabled={verifying}
          >
            {verifying ? 'Verifying…' : 'Verify Signature'}
          </button>
        </div>

        {verificationResult && (
          <div
            style={{
              padding: '0.75rem',
              borderRadius: '4px',
              marginBottom: '1rem',
              background: verificationResult === 'valid' ? '#064e3b' : '#7f1d1d',
              color: verificationResult === 'valid' ? '#6ee7b7' : '#fca5a5',
            }}
          >
            {verificationResult === 'valid'
              ? '✓ Valid: Receipt payload cryptographically matches the platform key signature.'
              : '✕ Invalid: Signature verification failed.'}
          </div>
        )}

        <div style={{ fontSize: '0.8rem' }}>
          <div style={{ marginBottom: '0.5rem' }}>
            <span style={{ color: '#9ca3af', display: 'block' }}>Receipt Bytes (Hex)</span>
            <code className="mono" style={{ wordBreak: 'break-all', display: 'block', background: '#1f2937', padding: '0.5rem', borderRadius: '4px' }}>
              {escrow.receipt?.bytes_hex || 'N/A'}
            </code>
          </div>
          <div style={{ marginBottom: '0.5rem' }}>
            <span style={{ color: '#9ca3af', display: 'block' }}>Platform Signature (Hex)</span>
            <code className="mono" style={{ wordBreak: 'break-all', display: 'block', background: '#1f2937', padding: '0.5rem', borderRadius: '4px' }}>
              {escrow.receipt?.signature_hex || 'N/A'}
            </code>
          </div>
          <div>
            <span style={{ color: '#9ca3af', display: 'block' }}>Platform Public Key (Hex)</span>
            <code className="mono" style={{ wordBreak: 'break-all', display: 'block', background: '#1f2937', padding: '0.5rem', borderRadius: '4px' }}>
              {escrow.receipt?.public_key_hex || 'N/A'}
            </code>
          </div>
        </div>
      </div>

      {/* Reject/Report Modal */}
      {showRejectModal && (
        <div
          style={{
            position: 'fixed',
            inset: 0,
            background: 'rgba(0,0,0,0.7)',
            display: 'flex',
            alignItems: 'center',
            justifyContent: 'center',
            zIndex: 1000,
          }}
        >
          <div
            className="panel"
            style={{
              background: '#1f2937',
              padding: '2rem',
              borderRadius: '8px',
              maxWidth: '480px',
              width: '100%',
            }}
          >
            <h3 style={{ marginTop: 0 }}>Report an Escrow Issue</h3>
            <form onSubmit={handleRejectSubmit}>
              <div style={{ marginBottom: '1rem' }}>
                <label style={{ display: 'block', marginBottom: '0.5rem', fontSize: '0.875rem' }}>
                  Reason:
                </label>
                <select
                  style={{
                    width: '100%',
                    background: '#111827',
                    border: '1px solid #4b5563',
                    color: '#fff',
                    padding: '0.5rem',
                    borderRadius: '4px',
                  }}
                  value={rejectReason}
                  onChange={(e) => setRejectReason(e.target.value)}
                >
                  <option value="pass_invalid">Pass is invalid or already redeemed</option>
                  <option value="wrong_tier">Wrong pass tier / event mismatch</option>
                  <option value="seller_unresponsive">Seller unresponsive / missing details</option>
                  <option value="other">Other issue</option>
                </select>
              </div>

              <div style={{ marginBottom: '1.25rem' }}>
                <label style={{ display: 'block', marginBottom: '0.5rem', fontSize: '0.875rem' }}>
                  Details / Note:
                </label>
                <textarea
                  style={{
                    width: '100%',
                    minHeight: '80px',
                    background: '#111827',
                    border: '1px solid #4b5563',
                    color: '#fff',
                    padding: '0.5rem',
                    borderRadius: '4px',
                  }}
                  value={rejectNote}
                  onChange={(e) => setRejectNote(e.target.value)}
                  placeholder="Explain why this pass cannot be accepted..."
                  required
                />
              </div>

              <div style={{ display: 'flex', justifyContent: 'flex-end', gap: '1rem' }}>
                <button
                  type="button"
                  className="ghost-button"
                  onClick={() => setShowRejectModal(false)}
                  disabled={actionBusy}
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  className="auth-button"
                  style={{ background: '#dc2626' }}
                  disabled={actionBusy}
                >
                  {actionBusy ? 'Submitting…' : 'Submit Dispute'}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}
    </div>
  )
}
