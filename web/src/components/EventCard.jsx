import { Link } from 'react-router-dom'
import { formatMoney, formatPercent, formatPrice, formatQty } from '../format'
import { spanOf } from '../lib/eventList'
import { bestAskTicks, bestBidTicks, lastTradeTicks } from '../lib/marketOverview'
import { ArrowRight } from './Icon'

/** "₹1,500.00" when all the numbers are alike, otherwise "₹1,500.00 to ₹4,000.00". */
const spanText = (values, format) => {
  const span = spanOf(values)
  if (!span) return '—'
  return span.low === span.high ? format(span.low) : `${format(span.low)} to ${format(span.high)}`
}

/**
 * One event: its name, venue, every tier with its face value and live prices, and the rules that
 * apply. Clicking it opens the trading screen, where the tiers are the tabs.
 *
 * @param {{
 *   event: import('../lib/eventList').EventEntry,
 *   snapshots: Record<number, any>,
 * }} props
 */
export function EventCard({ event, snapshots }) {
  const first = event.tiers[0]
  const escrowTiers = event.tiers.filter((tier) => tier.config.escrow_enabled).length
  const kind =
    escrowTiers === 0 ? 'Spot' : escrowTiers === event.tiers.length ? 'Escrow' : 'Spot + escrow'
  const many = event.tiers.length > 1

  return (
    <Link to={`/markets/${first.marketId}`} className="event-card" aria-label={`Trade ${event.name}`}>
      <div className="event-card-top">
        <span className="market-kind">{kind}</span>
        <span className="event-card-id">E{event.eventId}</span>
      </div>

      <div className="event-card-title">
        <h3>{event.name}</h3>
        {event.venue && <span className="event-card-venue">{event.venue}</span>}
      </div>

      <ul className="event-card-tiers">
        {event.tiers.map((tier) => {
          const snapshot = snapshots[tier.marketId]
          const last = lastTradeTicks(snapshot)
          const bid = bestBidTicks(snapshot)
          const ask = bestAskTicks(snapshot)
          const price = (ticks) => (ticks != null ? formatPrice(tier.config, ticks) : '—')
          return (
            <li key={tier.marketId}>
              <div className="event-tier-head">
                <span>{many ? tier.name : 'Face value'}</span>
                <strong>{formatMoney(tier.config.face_value_minor)}</strong>
              </div>
              <div className="event-tier-stats">
                <span>
                  Last <b>{last != null ? price(last) : 'no prints'}</b>
                </span>
                <span>
                  Bid <b className="positive">{price(bid)}</b>
                </span>
                <span>
                  Ask <b className="negative">{price(ask)}</b>
                </span>
              </div>
            </li>
          )
        })}
      </ul>

      <div className="event-card-foot">
        <span>
          Fee {spanText(event.tiers.map((tier) => tier.config.taker_fee_bps), formatPercent)} · Limit{' '}
          {spanText(event.tiers.map((tier) => tier.config.holding_limit), formatQty)} passes
        </span>
        <span className="event-card-open">
          Trade <ArrowRight />
        </span>
      </div>
    </Link>
  )
}