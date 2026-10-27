import { useEffect, useState } from 'react'
import { previewCollar } from '../adminApi'
import { formatMoney } from '../format'
import { rupeesToMinor } from '../rupees'
import { useAdminStore } from '../stores/useAdminStore'

const STATUS_TEXT = { draft: 'Draft', publishing: 'Publishing…', published: 'Published' }

const rangeText = (tier) => `${formatMoney(tier.floor_minor)} – ${formatMoney(tier.cap_minor)}`

function EventForm() {
  const addEvent = useAdminStore((state) => state.addEvent)
  const busy = useAdminStore((state) => state.busy)
  const [name, setName] = useState('')
  const [venue, setVenue] = useState('')

  const handleSubmit = async (event) => {
    event.preventDefault()
    if (await addEvent(name.trim(), venue.trim())) {
      setName('')
      setVenue('')
    }
  }

  return (
    <form className="panel admin-card" onSubmit={handleSubmit}>
      <h3>Create an event</h3>
      <div className="admin-fields">
        <label className="field">
          <span>Event name</span>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={80} required />
        </label>
        <label className="field">
          <span>Venue (optional)</span>
          <input value={venue} onChange={(e) => setVenue(e.target.value)} maxLength={80} />
        </label>
      </div>
      <button type="submit" className="auth-button admin-button" disabled={busy}>
        Create event
      </button>
    </form>
  )
}

function TierForm({ eventId }) {
  const addTier = useAdminStore((state) => state.addTier)
  const busy = useAdminStore((state) => state.busy)
  const [name, setName] = useState('')
  const [face, setFace] = useState('')
  const [escrowEnabled, setEscrowEnabled] = useState(false)
  // The server's answer is stored together with the amount it was asked about, so an old answer
  // is never shown next to a newer amount.
  const [preview, setPreview] = useState(null)

  const faceMinor = rupeesToMinor(face)

  // Ask the server for the price range 300ms after the admin stops typing.
  useEffect(() => {
    if (faceMinor === null) return undefined
    let cancelled = false
    const timer = setTimeout(async () => {
      try {
        const collar = await previewCollar(faceMinor)
        if (!cancelled) setPreview({ faceMinor, collar })
      } catch (err) {
        if (!cancelled) setPreview({ faceMinor, collar: null, reason: err?.reason })
      }
    }, 300)
    return () => {
      cancelled = true
      clearTimeout(timer)
    }
  }, [faceMinor])

  const current = preview?.faceMinor === faceMinor ? preview : null

  const handleSubmit = async (event) => {
    event.preventDefault()
    if (faceMinor === null) return
    if (await addTier(eventId, name.trim(), faceMinor, escrowEnabled)) {
      setName('')
      setFace('')
      setEscrowEnabled(false)
    }
  }

  return (
    <form className="admin-tier-form" onSubmit={handleSubmit}>
      <label className="field">
        <span>Tier name</span>
        <input value={name} onChange={(e) => setName(e.target.value)} maxLength={80} required />
      </label>
      <label className="field">
        <span>Face value (₹)</span>
        <input value={face} onChange={(e) => setFace(e.target.value)} inputMode="decimal" required />
      </label>
      <label className="field admin-checkbox">
        <input
          type="checkbox"
          checked={escrowEnabled}
          onChange={(e) => setEscrowEnabled(e.target.checked)}
        />
        <span>Hold fills in escrow until the seller signs delivery</span>
      </label>
      <p className="admin-collar" aria-live="polite">
        {faceMinor === null && face !== '' && 'Enter an amount like 5000 or 5000.50.'}
        {current?.collar && <>Allowed price range: <strong>{rangeText(current.collar)}</strong> (set by the system)</>}
        {current && !current.collar && 'This face value is not allowed (it must be a multiple of the tick).'}
      </p>
      <button type="submit" className="auth-button admin-button" disabled={busy || faceMinor === null}>
        Add tier
      </button>
    </form>
  )
}

function TierRow({ tier }) {
  const publish = useAdminStore((state) => state.publish)
  const busy = useAdminStore((state) => state.busy)
  const isDraft = tier.status === 'draft'

  return (
    <tr>
      <td>{tier.name}</td>
      <td className="mono">{formatMoney(tier.face_value_minor)}</td>
      <td className="mono">{rangeText(tier)}</td>
      <td className="mono">{tier.holding_limit}</td>
      <td>{tier.escrow_enabled ? 'On' : 'Off'}</td>
      <td>
        <span className={`admin-badge ${tier.status}`}>{STATUS_TEXT[tier.status] ?? tier.status}</span>
      </td>
      <td>
        {isDraft && (
          <button type="button" className="ghost-button" disabled={busy} onClick={() => publish(tier.market_id)}>
            Publish
          </button>
        )}
      </td>
    </tr>
  )
}

function EventCard({ event }) {
  return (
    <section className="panel admin-card">
      <h3>
        {event.name}
        {event.venue && <span className="admin-venue"> · {event.venue}</span>}
      </h3>
      {event.tiers.length === 0 ? (
        <p className="admin-empty">No tiers yet. Add one below.</p>
      ) : (
        <table className="admin-table">
          <thead>
            <tr>
              <th>Tier</th>
              <th>Face value</th>
              <th>Price range</th>
              <th>Holding limit</th>
              <th>Escrow</th>
              <th>Status</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {event.tiers.map((tier) => (
              <TierRow key={tier.market_id} tier={tier} />
            ))}
          </tbody>
        </table>
      )}
      <TierForm eventId={event.event_id} />
    </section>
  )
}

export function AdminPage() {
  const events = useAdminStore((state) => state.events)
  const status = useAdminStore((state) => state.status)
  const error = useAdminStore((state) => state.error)
  const load = useAdminStore((state) => state.load)

  useEffect(() => {
    load()
  }, [load])

  return (
    <div className="admin-page">
      <h2>Admin console</h2>
      <p className="admin-intro">
        Create an event, add price tiers, then publish. The allowed price range is worked out by the
        system from the face value. Publishing locks a tier.
      </p>
      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}
      <EventForm />
      {status === 'loading' ? (
        <p className="admin-empty">Loading…</p>
      ) : (
        events.map((event) => <EventCard key={event.event_id} event={event} />)
      )}
    </div>
  )
}