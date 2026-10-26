import { describe, expect, it } from 'vitest'
import {
  commandsPerSecond,
  createLatencySample,
  formatMicros,
  formatMilliseconds,
  percentile,
  summarizeLatency,
} from './latency.js'

describe('latency helpers', () => {
  it('uses nearest-rank percentiles', () => {
    expect(percentile([], 99)).toBe(0)
    expect(percentile([4, 1, 3, 2], 50)).toBe(2)
    expect(percentile([4, 1, 3, 2], 99)).toBe(4)
    expect(percentile([1, 2], 0)).toBe(1)
  })

  it('derives acknowledged, visible, and render times from timestamps', () => {
    const sample = createLatencySample(100, 108.5, 120)

    expect(sample.ackMs).toBeCloseTo(8.5)
    expect(sample.visibleMs).toBeCloseTo(20)
    expect(sample.renderMs).toBeCloseTo(11.5)
  })

  it('never reports a visible time before the acknowledgement', () => {
    const sample = createLatencySample(100, 120, 110)

    expect(sample.ackMs).toBe(20)
    expect(sample.visibleMs).toBe(20)
    expect(sample.renderMs).toBe(0)
  })

  it('summarizes the client samples for the panel', () => {
    const samples = [
      createLatencySample(0, 4, 10),
      createLatencySample(0, 8, 18),
      createLatencySample(0, 12, 30),
    ]
    const summary = summarizeLatency(samples)

    expect(summary.count).toBe(3)
    expect(summary.ackP50).toBe(8)
    expect(summary.ackP99).toBe(12)
    expect(summary.visibleP50).toBe(18)
    expect(summary.visibleP99).toBe(30)
    expect(summary.maxVisible).toBe(30)
  })

  it('calculates command throughput and guards bad polling ranges', () => {
    expect(commandsPerSecond(100, 250, 1_000)).toBe(150)
    expect(commandsPerSecond(250, 100, 1_000)).toBe(0)
    expect(commandsPerSecond(100, 250, 0)).toBe(0)
  })

  it('formats microsecond and millisecond values', () => {
    expect(formatMicros(850)).toBe('850 µs')
    expect(formatMicros(1_500)).toBe('1.50 ms')
    expect(formatMicros(12_500)).toBe('12.5 ms')
    expect(formatMicros(null)).toBe('—')
    expect(formatMilliseconds(8.456)).toBe('8.46 ms')
    expect(formatMilliseconds(18.46)).toBe('18.5 ms')
    expect(formatMilliseconds(undefined)).toBe('—')
  })
})
