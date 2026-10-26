// @ts-check
/**
 * Latency sampling and percentile helpers for the T1 speed panel.
 *
 * All samples use the browser's monotonic clock. The panel shows the client
 * round trip separately from the gateway's own queue and core histograms.
 */

/**
 * @typedef {{
 *   acknowledgedAt: number,
 *   visibleAt: number,
 *   ackMs: number,
 *   visibleMs: number,
 *   renderMs: number,
 *   at: number,
 * }} CommandLatencySample
 */

/**
 * @typedef {{
 *   count: number,
 *   ackP50: number,
 *   ackP99: number,
 *   visibleP50: number,
 *   visibleP99: number,
 *   maxVisible: number,
 * }} LatencySummary
 */

/**
 * Nearest-rank percentile. Returns zero for an empty input.
 *
 * @param {readonly number[]} values
 * @param {number} p
 * @returns {number}
 */
export const percentile = (values, p) => {
  if (values.length === 0) return 0
  const ordered = values
    .filter((value) => Number.isFinite(value))
    .sort((left, right) => left - right)
  if (ordered.length === 0) return 0

  const bounded = Math.min(100, Math.max(0, p))
  const rank = Math.ceil((bounded / 100) * ordered.length)
  return ordered[Math.min(ordered.length - 1, Math.max(0, rank - 1))]
}

/**
 * Turn three monotonic timestamps into the three values shown by the panel.
 *
 * @param {number} sentAt
 * @param {number} acknowledgedAt
 * @param {number} visibleAt
 * @returns {CommandLatencySample}
 */
export const createLatencySample = (sentAt, acknowledgedAt, visibleAt) => {
  const ackMs = Math.max(0, acknowledgedAt - sentAt)
  const visibleMs = Math.max(ackMs, visibleAt - sentAt)
  return {
    acknowledgedAt,
    visibleAt,
    ackMs,
    visibleMs,
    renderMs: Math.max(0, visibleMs - ackMs),
    at: Date.now(),
  }
}

/**
 * @param {readonly CommandLatencySample[]} [samples]
 * @returns {LatencySummary}
 */
export const summarizeLatency = (samples = []) => {
  const ack = samples.map((sample) => sample.ackMs)
  const visible = samples.map((sample) => sample.visibleMs)
  return {
    count: samples.length,
    ackP50: percentile(ack, 50),
    ackP99: percentile(ack, 99),
    visibleP50: percentile(visible, 50),
    visibleP99: percentile(visible, 99),
    maxVisible: visible.length > 0 ? Math.max(...visible) : 0,
  }
}

/**
 * @param {number} previousTotal
 * @param {number} currentTotal
 * @param {number} elapsedMs
 * @returns {number}
 */
export const commandsPerSecond = (previousTotal, currentTotal, elapsedMs) => {
  if (
    !Number.isFinite(previousTotal) ||
    !Number.isFinite(currentTotal) ||
    !Number.isFinite(elapsedMs) ||
    elapsedMs <= 0 ||
    currentTotal < previousTotal
  ) {
    return 0
  }
  return ((currentTotal - previousTotal) * 1000) / elapsedMs
}

/**
 * Gateway histograms are stored in microseconds.
 *
 * @param {number | null | undefined} micros
 * @returns {string}
 */
export const formatMicros = (micros) => {
  if (micros == null || !Number.isFinite(micros)) return '—'
  if (micros < 1_000) return `${Math.round(micros)} µs`
  return `${(micros / 1_000).toFixed(micros >= 10_000 ? 1 : 2)} ms`
}

/**
 * @param {number | null | undefined} milliseconds
 * @returns {string}
 */
export const formatMilliseconds = (milliseconds) => {
  if (milliseconds == null || !Number.isFinite(milliseconds)) return '—'
  if (milliseconds < 10) return `${milliseconds.toFixed(2)} ms`
  return `${milliseconds.toFixed(1)} ms`
}
