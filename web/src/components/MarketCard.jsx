import { Link } from 'react-router-dom'
import {
  formatMoney,
  formatPercent,
  formatPrice,
  formatQty,
  marketBounds,
} from '../format'
import { bestAskTicks, bestBidTicks, lastTradeTicks } from '../lib/marketOverview'
import { ArrowRight } from './Icon'

/**
 * One market card, used on the landing page and the markets index.
 *
 * @param {{ view: import('../protocol/generated/MarketView').MarketView }} props
 */
export function MarketCard({ view }) {
  const market = view.config
  const snapshot = view.snapshot
  const bounds = marketBounds(market)
  const last = lastTradeTicks(snapshot)
  const bestBid = bestBidTicks(snapshot)
  const bestAsk = bestAskTicks(snapshot)

  const bids = snapshot?.bids ?? []
  const asks = snapshot?.asks ?? []
  const bidVolume = bids.reduce((sum, level) => sum + Number(level.qty), 0)
  const askVolume = asks.reduce((sum, level) => sum + Number(level.qty), 0)
  const totalVolume = bidVolume + askVolume
  const bidShare =
    totalVolume > 0 ? Math.round((bidVolume / totalVolume) * 100) : null

  return (
    <Link
      to={`/markets/${market.id}`}
      className="market-card"
      aria-label={`Trade ${market.name}`}
    >
      <div className="market-card-top">
        <span className="market-kind">
          {market.escrow_enabled ? 'Escrow' : 'Spot'}
        </span>
        <span className="market-card-collar">
          Collar {formatPrice(market, bounds.floor)} – {formatPrice(market, bounds.cap)}
        </span>
      </div>

      <div className="market-card-title">
        <h3>{market.name}</h3>
        <span className="market-card-id">M{market.id}</span>
      </div>

      <div className="market-card-last">
        <span>Last print</span>
        <strong>{last != null ? formatPrice(market, last) : 'No prints yet'}</strong>
      </div>

      <dl className="market-card-stats">
        <div>
          <dt>Best bid</dt>
          <dd className="positive">
            {bestBid != null ? formatPrice(market, bestBid) : '—'}
          </dd>
        </div>
        <div>
          <dt>Best ask</dt>
          <dd className="negative">
            {bestAsk != null ? formatPrice(market, bestAsk) : '—'}
          </dd>
        </div>
        <div>
          <dt>Taker fee</dt>
          <dd>{formatPercent(market.taker_fee_bps)}</dd>
        </div>
        <div>
          <dt>Holding limit</dt>
          <dd>{formatQty(market.holding_limit)} passes</dd>
        </div>
      </dl>

      <div className="market-volume">
        {bidShare == null ? (
          <div className="market-volume-empty">No resting liquidity</div>
        ) : (
          <>
            <div className="market-volume-bars" aria-hidden="true">
              <span className="bid" style={{ width: `${bidShare}%` }} />
              <span className="ask" style={{ width: `${100 - bidShare}%` }} />
            </div>
            <div className="market-volume-labels">
              <span>Bid vol {bidShare}%</span>
              <span>Ask vol {100 - bidShare}%</span>
            </div>
          </>
        )}
      </div>

      <p className="market-card-face">
        {formatMoney(market.face_value_minor)} face value ·{' '}
        {formatQty(market.holding_limit)} pass limit
      </p>

      <span className="market-card-action">
        Trade
        <ArrowRight size={13} />
      </span>
    </Link>
  )
}
