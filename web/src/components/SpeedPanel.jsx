import { useEffect, useMemo, useRef, useState } from 'react'
import { getLatencyMetrics } from '../api'
import {
  commandsPerSecond,
  formatMicros,
  formatMilliseconds,
  summarizeLatency,
} from '../lib/latency'

const POLL_MS = 1_500
const MAX_HISTORY_ROWS = 12

/**
 * @param {{
 *   variant?: 'market' | 'page',
 *   samples?: import('../lib/latency').CommandLatencySample[],
 *   lastSample?: import('../lib/latency').CommandLatencySample | null,
 *   onClear?: () => void,
 * }} props
 */
export function SpeedPanel({
  variant = 'market',
  samples = [],
  lastSample = null,
  onClear,
}) {
  const [server, setServer] = useState(null)
  const [rate, setRate] = useState(null)
  const [error, setError] = useState(null)
  const previousPoll = useRef(null)
  const summary = useMemo(() => summarizeLatency(samples), [samples])
  const isPage = variant === 'page'

  useEffect(() => {
    let cancelled = false
    let timer = 0

    const poll = async () => {
      try {
        const next = await getLatencyMetrics()
        if (cancelled) return

        const now = performance.now()
        if (previousPoll.current) {
          setRate(
            commandsPerSecond(
              previousPoll.current.commandsTotal,
              next.commands_total,
              now - previousPoll.current.at,
            ),
          )
        }
        previousPoll.current = {
          commandsTotal: next.commands_total,
          at: now,
        }
        setServer(next)
        setError(null)
      } catch (pollError) {
        if (!cancelled) {
          setError(
            pollError instanceof Error ? pollError.message : String(pollError),
          )
        }
      }
    }

    poll()
    timer = window.setInterval(poll, POLL_MS)
    return () => {
      cancelled = true
      window.clearInterval(timer)
    }
  }, [])

  const recentSamples = samples.slice(-MAX_HISTORY_ROWS).reverse()

  return (
    <section className={`panel speed-panel ${variant}`}>
      <div className="panel-header">
        <h3>Speed panel</h3>
        <span className="panel-meta">
          {server ? `Gateway seq ${server.seq}` : 'Waiting for gateway'}
        </span>
      </div>

      <div className="speed-body">
        <div className="latency-chain" aria-label="Last order latency">
          <LatencyStep label="Sent" value="0.00 ms" caption="order form" />
          <span className="latency-arrow" aria-hidden="true">
            →
          </span>
          <LatencyStep
            label="Acknowledged"
            value={lastSample ? formatMilliseconds(lastSample.ackMs) : '—'}
            caption="REST response"
            tone="ack"
          />
          <span className="latency-arrow" aria-hidden="true">
            →
          </span>
          <LatencyStep
            label="Visible"
            value={lastSample ? formatMilliseconds(lastSample.visibleMs) : '—'}
            caption={
              lastSample
                ? `${formatMilliseconds(lastSample.renderMs)} after ack`
                : 'next painted frame'
            }
            tone="visible"
          />
        </div>

        <div className="latency-summary-grid">
          <SmallMetric
            label="Ack p50"
            value={
              summary.count > 0 ? formatMilliseconds(summary.ackP50) : '—'
            }
          />
          <SmallMetric
            label="Ack p99"
            value={
              summary.count > 0 ? formatMilliseconds(summary.ackP99) : '—'
            }
          />
          <SmallMetric
            label="Visible p50"
            value={
              summary.count > 0
                ? formatMilliseconds(summary.visibleP50)
                : '—'
            }
          />
          <SmallMetric
            label="Visible p99"
            value={
              summary.count > 0
                ? formatMilliseconds(summary.visibleP99)
                : '—'
            }
          />
          <SmallMetric
            label="Slowest"
            value={
              summary.count > 0
                ? formatMilliseconds(summary.maxVisible)
                : '—'
            }
          />
          <SmallMetric label="Orders sampled" value={String(summary.count)} />
        </div>

        <div className="speed-subhead">
          <span>Gateway histograms</span>
          <span>polled every {POLL_MS / 1_000}s</span>
        </div>

        <div className="speed-metrics">
          <SpeedMetric
            label="Core p99"
            value={server ? formatMicros(server.core_p99_us) : '—'}
            hint="matching"
          />
          <SpeedMetric
            label="Queue wait p99"
            value={server ? formatMicros(server.queue_wait_p99_us) : '—'}
            hint="admission"
          />
          <SpeedMetric
            label="Queue depth"
            value={server ? String(server.queue_depth) : '—'}
            hint="jobs"
          />
          <SpeedMetric
            label="Throughput"
            value={rate == null ? '—' : `${Math.round(rate)}/s`}
            hint="commands"
          />
          <SpeedMetric
            label="Commands"
            value={server ? String(server.commands_total) : '—'}
            hint="total"
          />
          <SpeedMetric
            label="Rejected"
            value={server ? String(server.rejected_total) : '—'}
            hint="total"
          />
          <SpeedMetric
            label="WS clients"
            value={server ? String(server.ws_clients) : '—'}
            hint="connected"
          />
          <SpeedMetric
            label="Slow consumers"
            value={server ? String(server.slow_consumers) : '—'}
            hint="disconnected"
          />
        </div>

        {error && <p className="speed-error">Metrics unavailable: {error}</p>}

        {isPage && (
          <div className="latency-history">
            <div className="latency-history-head">
              <span>Latest orders</span>
              {onClear && samples.length > 0 && (
                <button type="button" className="ghost-button" onClick={onClear}>
                  Clear samples
                </button>
              )}
            </div>
            {recentSamples.length === 0 ? (
              <div className="empty-row">
                No orders measured yet. Place or cancel an order to populate the
                panel.
              </div>
            ) : (
              <div className="latency-table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th>Time</th>
                      <th>Sent → ack</th>
                      <th>Sent → visible</th>
                      <th>Ack → visible</th>
                    </tr>
                  </thead>
                  <tbody>
                    {recentSamples.map((sample) => (
                      <tr key={`${sample.at}-${sample.visibleAt}`}>
                        <td>
                          {new Date(sample.at).toLocaleTimeString([], {
                            hour12: false,
                          })}
                        </td>
                        <td>{formatMilliseconds(sample.ackMs)}</td>
                        <td>{formatMilliseconds(sample.visibleMs)}</td>
                        <td>{formatMilliseconds(sample.renderMs)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
            <p className="speed-note">
              Browser timings use <code>performance.now()</code>, so they need no
              server-clock offset. Gateway histograms are measured on the
              single-writer queue and matching core.
            </p>
          </div>
        )}
      </div>
    </section>
  )
}

/**
 * @param {{
 *   label: string,
 *   value: string,
 *   caption: string,
 *   tone?: 'neutral' | 'ack' | 'visible',
 * }} props
 */
function LatencyStep({ label, value, caption, tone = 'neutral' }) {
  return (
    <div className={`latency-step ${tone}`}>
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{caption}</small>
    </div>
  )
}

/**
 * @param {{ label: string, value: string }} props
 */
function SmallMetric({ label, value }) {
  return (
    <div className="latency-small">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  )
}

/**
 * @param {{ label: string, value: string, hint: string }} props
 */
function SpeedMetric({ label, value, hint }) {
  return (
    <div className="speed-metric">
      <span>{label}</span>
      <strong>{value}</strong>
      <small>{hint}</small>
    </div>
  )
}
