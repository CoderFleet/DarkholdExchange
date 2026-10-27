import { useEffect, useState, useCallback } from 'react'
import { getAdminDisputes, ruleAdminDispute } from '../api'

export function AdminDisputesPage() {
  const [disputes, setDisputes] = useState([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState(null)
  const [selectedDispute, setSelectedDispute] = useState(null)
  const [modalRuling, setModalRuling] = useState(null) // 'buyer' | 'seller'
  const [adminNote, setAdminNote] = useState('')
  const [submitting, setSubmitting] = useState(false)
  const [actionSuccess, setActionSuccess] = useState('')

  const fetchDisputes = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const res = await getAdminDisputes()
      setDisputes(res || [])
    } catch (err) {
      setError(err.message || 'Failed to fetch disputes')
    } finally {
      setLoading(false)
    }
  }, [])
  
  useEffect(() => {
    const timer = setTimeout(() => {
      fetchDisputes()
    }, 0)
    return () => clearTimeout(timer)
  }, [fetchDisputes])
  
  const handleOpenModal = (dispute, ruling) => {
    setSelectedDispute(dispute)
    setModalRuling(ruling)
    setAdminNote('')
    setActionSuccess('')
  }

  const handleCloseModal = () => {
    setSelectedDispute(null)
    setModalRuling(null)
    setAdminNote('')
  }

  const handleConfirmRuling = async (e) => {
    e.preventDefault()
    if (!selectedDispute || !modalRuling) return
    if (!adminNote.trim()) {
      alert('Admin note is required.')
      return
    }

    setSubmitting(true)
    try {
      await ruleAdminDispute(selectedDispute.id, modalRuling, adminNote.trim())
      setActionSuccess(`Dispute #${selectedDispute.id} ruled in favor of ${modalRuling}! Posted as new ledger entry.`)
      handleCloseModal()
      await fetchDisputes()
    } catch (err) {
      alert(err.message || 'Failed to submit ruling')
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="admin-disputes-page" style={{ padding: '2rem', maxWidth: '1100px', margin: '0 auto' }}>
      <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', marginBottom: '1.5rem' }}>
        <div>
          <span className="eyebrow">Admin Console</span>
          <h2>Escrow Disputes</h2>
        </div>
        <button type="button" className="ghost-button" onClick={fetchDisputes} disabled={loading}>
          {loading ? 'Refreshing…' : '↻ Refresh'}
        </button>
      </div>

      {actionSuccess && (
        <div className="banner success" style={{ marginBottom: '1.5rem', background: '#064e3b', color: '#6ee7b7', padding: '1rem', borderRadius: '6px' }}>
          {actionSuccess}
        </div>
      )}

      {error && (
        <div className="banner error" role="alert" style={{ marginBottom: '1.5rem', background: '#7f1d1d', color: '#fca5a5', padding: '1rem', borderRadius: '6px' }}>
          {error}
        </div>
      )}

      {loading ? (
        <p>Loading disputes…</p>
      ) : disputes.length === 0 ? (
        <div className="panel" style={{ padding: '2rem', textAlign: 'center' }}>
          <p>No disputes recorded.</p>
        </div>
      ) : (
        <div className="disputes-list" style={{ display: 'grid', gap: '1.5rem' }}>
          {disputes.map((dispute) => {
            const isResolved = dispute.status === 'resolved'
            return (
              <div
                key={dispute.id}
                className="panel admin-card"
                style={{
                  border: isResolved ? '1px solid #374151' : '1px solid #f59e0b',
                  padding: '1.5rem',
                  borderRadius: '8px',
                  background: '#111827',
                }}
              >
                <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start', marginBottom: '1rem' }}>
                  <div>
                    <h3 style={{ margin: 0, fontSize: '1.25rem' }}>
                      Dispute #{dispute.id} (Escrow #{dispute.escrow_id})
                    </h3>
                    <span style={{ fontSize: '0.875rem', color: '#9ca3af' }}>
                      Status: <strong style={{ color: isResolved ? '#10b981' : '#f59e0b' }}>{dispute.status.toUpperCase()}</strong>
                    </span>
                  </div>
                  {isResolved && (
                    <span style={{ background: '#1f2937', padding: '0.25rem 0.75rem', borderRadius: '4px', fontSize: '0.875rem' }}>
                      Ruling: <strong>{dispute.ruling?.toUpperCase()}</strong>
                    </span>
                  )}
                </div>

                <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(200px, 1fr))', gap: '1rem', marginBottom: '1rem', background: '#1f2937', padding: '1rem', borderRadius: '6px' }}>
                  <div>
                    <span style={{ color: '#9ca3af', display: 'block', fontSize: '0.75rem' }}>Reason</span>
                    <strong style={{ fontSize: '0.95rem' }}>{dispute.reason}</strong>
                  </div>
                  <div>
                    <span style={{ color: '#9ca3af', display: 'block', fontSize: '0.75rem' }}>Buyer Note</span>
                    <span style={{ fontSize: '0.95rem' }}>{dispute.note || 'None'}</span>
                  </div>
                  {dispute.ruled_by && (
                    <div>
                      <span style={{ color: '#9ca3af', display: 'block', fontSize: '0.75rem' }}>Ruled By Admin</span>
                      <span className="mono">User #{dispute.ruled_by}</span>
                    </div>
                  )}
                </div>

                {!isResolved && (
                  <div style={{ display: 'flex', gap: '1rem', marginTop: '1rem' }}>
                    <button
                      type="button"
                      className="auth-button"
                      style={{ background: '#2563eb', padding: '0.5rem 1rem', borderRadius: '4px', cursor: 'pointer' }}
                      onClick={() => handleOpenModal(dispute, 'buyer')}
                    >
                      Rule for Buyer (Refund)
                    </button>
                    <button
                      type="button"
                      className="auth-button"
                      style={{ background: '#059669', padding: '0.5rem 1rem', borderRadius: '4px', cursor: 'pointer' }}
                      onClick={() => handleOpenModal(dispute, 'seller')}
                    >
                      Rule for Seller (Release)
                    </button>
                  </div>
                )}
              </div>
            )
          })}
        </div>
      )}

      {/* Confirmation Modal */}
      {selectedDispute && modalRuling && (
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
              maxWidth: '520px',
              width: '100%',
              boxShadow: '0 20px 25px -5px rgba(0,0,0,0.5)',
            }}
          >
            <h3 style={{ marginTop: 0 }}>
              Confirm Ruling: {modalRuling === 'buyer' ? 'Refund Buyer' : 'Release to Seller'}
            </h3>
            <p style={{ color: '#fbbf24', fontSize: '0.9rem', marginBottom: '1rem' }}>
              ⚠️ Notice: This ruling is final and posted as a <strong>new ledger entry</strong>. Release and refund are mutually exclusive and can only happen once.
            </p>

            <form onSubmit={handleConfirmRuling}>
              <div style={{ marginBottom: '1.25rem' }}>
                <label style={{ display: 'block', marginBottom: '0.5rem', fontSize: '0.875rem' }}>
                  Admin Note / Explanation (Required):
                </label>
                <textarea
                  style={{
                    width: '100%',
                    minHeight: '80px',
                    background: '#111827',
                    border: '1px solid #4b5563',
                    borderRadius: '4px',
                    color: '#fff',
                    padding: '0.5rem',
                  }}
                  value={adminNote}
                  onChange={(e) => setAdminNote(e.target.value)}
                  placeholder="State the reason and findings for this ruling..."
                  required
                />
              </div>

              <div style={{ display: 'flex', justifyContent: 'flex-end', gap: '1rem' }}>
                <button
                  type="button"
                  className="ghost-button"
                  onClick={handleCloseModal}
                  disabled={submitting}
                >
                  Cancel
                </button>
                <button
                  type="submit"
                  className="auth-button"
                  style={{
                    background: modalRuling === 'buyer' ? '#2563eb' : '#059669',
                    padding: '0.5rem 1.25rem',
                  }}
                  disabled={submitting}
                >
                  {submitting ? 'Posting Ledger Entry…' : 'Confirm Ruling'}
                </button>
              </div>
            </form>
          </div>
        </div>
      )}
    </div>
  )
}
