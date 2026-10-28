import { useMemo } from 'react'
import { Link } from 'react-router-dom'
import { formatMoney, formatPercent, formatPrice } from '../format'
import { buildEventList, eventOfMarket } from '../lib/eventList'
import { useEvents } from '../lib/useEvents'

/**
 * The tabs above the trading screen: only the tiers of the event being traded, so a trader can
 * switch from Gold to General without leaving the event. An event with a single tier needs no tabs.
 *
 * @param {{
 *   markets: import('../lib/eventList').MarketLike[],
 *   snapshots: Record<number, any>,
 *   selectedMarketId: number | null,
 * }} props
 */
export function TierTabs({ markets, snapshots, selectedMarketId }) {
  const events = useEvents()
  const event = useMemo(
    () => eventOfMarket(buildEventList(events, markets), selectedMarketId),
    [events, markets, selectedMarketId],
  )
  if (!event || event.tiers.length < 2) return null

  return (
    <nav className="market-tabs" aria-label={`${event.name} tiers`}>
      {event.tiers.map((tier) => {
        const snapshot = snapshots[tier.marketId]
        const active = tier.marketId === selectedMarketId
        return (
          <Link
            key={tier.marketId}
            to={`/markets/${tier.marketId}`}
            className={`market-tab ${active ? 'active' : ''}`}
            aria-current={active ? 'page' : undefined}
          >
            <span className="market-tab-top">
              <span>{tier.name}</span>
              <span className="market-tab-id">M{tier.marketId}</span>
            </span>
            <span className="market-tab-meta">
              {formatMoney(tier.config.face_value_minor)} face · {formatPercent(tier.config.taker_fee_bps)} fee
            </span>
            <span className={`market-tab-last ${snapshot?.last_trade_price ? 'has-trade' : ''}`}>
              {snapshot?.last_trade_price
                ? formatPrice(tier.config, snapshot.last_trade_price)
                : 'No prints yet'}
            </span>
          </Link>
        )
      })}
    </nav>
  )
}