import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  AreaSeries,
  BarSeries,
  CandlestickSeries,
  ColorType,
  CrosshairMode,
  createChart,
  LineSeries,
} from 'lightweight-charts'
import {
  buildDepthSeries,
  buildTradeChartData,
  padDomain,
} from '../charts/chartData'
import { formatPrice, formatQty } from '../format'

const GREEN = '#43d98b'
const RED = '#ff6b81'
const PURPLE = '#b388ff'
const GRID = 'rgba(255, 255, 255, 0.065)'
const MUTED = '#7c7e95'
const FONT = '10px ui-monospace, SFMono-Regular, Menlo, Monaco, monospace'

const DEPTH_HEIGHT = 220
const CANDLE_SEQUENCE_GROUP = 8

const CHART_TYPES = [
  { id: 'area', label: 'Area' },
  { id: 'line', label: 'Line' },
  { id: 'candles', label: 'Candles' },
  { id: 'bars', label: 'Bars' },
]

/**
 * Size the backing store for the current device pixel ratio and return a 2D
 * context whose drawing units are CSS pixels.
 *
 * @param {HTMLCanvasElement} canvas
 * @param {number} width
 * @param {number} height
 * @returns {CanvasRenderingContext2D | null}
 */
const prepareCanvas = (canvas, width, height) => {
  const ratio = Math.min(window.devicePixelRatio || 1, 2)
  canvas.width = Math.max(1, Math.round(width * ratio))
  canvas.height = Math.max(1, Math.round(height * ratio))
  canvas.style.width = `${width}px`
  canvas.style.height = `${height}px`

  const context = canvas.getContext('2d')
  if (!context) return null
  context.setTransform(ratio, 0, 0, ratio, 0, 0)
  context.clearRect(0, 0, width, height)
  return context
}

/**
 * @param {number} height
 * @param {(canvas: HTMLCanvasElement, width: number, height: number) => void} draw
 */
const useResponsiveCanvas = (height, draw) => {
  const canvasRef = useRef(null)
  const wrapRef = useRef(null)

  useEffect(() => {
    const wrap = wrapRef.current
    const canvas = canvasRef.current
    if (!wrap || !canvas) return undefined

    let frame = 0
    const render = () => {
      cancelAnimationFrame(frame)
      frame = requestAnimationFrame(() => {
        const width = wrap.clientWidth
        if (width > 0) draw(canvas, width, height)
      })
    }

    render()
    const observer = new ResizeObserver(render)
    observer.observe(wrap)
    return () => {
      cancelAnimationFrame(frame)
      observer.disconnect()
    }
  }, [draw, height])

  return { canvasRef, wrapRef }
}

/**
 * @param {CanvasRenderingContext2D} context
 * @param {number} left
 * @param {number} right
 * @param {number} top
 * @param {number} bottom
 * @param {number} rows
 */
const drawGrid = (context, left, right, top, bottom, rows) => {
  context.save()
  context.strokeStyle = GRID
  context.lineWidth = 1
  context.font = FONT
  context.fillStyle = MUTED

  for (let row = 0; row <= rows; row += 1) {
    const y = top + ((bottom - top) * row) / rows
    context.beginPath()
    context.moveTo(left, y + 0.5)
    context.lineTo(right, y + 0.5)
    context.stroke()
  }
  context.restore()
}

/**
 * @param {HTMLCanvasElement} canvas
 * @param {number} width
 * @param {number} height
 * @param {import('../charts/chartData').DepthSeries} series
 * @param {import('../protocol/generated/MarketConfig').MarketConfig} market
 */
const drawDepthChart = (canvas, width, height, series, market) => {
  const context = prepareCanvas(canvas, width, height)
  if (!context) return

  const left = 62
  const right = 16
  const top = 16
  const bottom = 30
  const plotWidth = Math.max(1, width - left - right)
  const plotHeight = Math.max(1, height - top - bottom)
  const plotRight = left + plotWidth
  const plotBottom = top + plotHeight

  drawGrid(context, left, plotRight, top, plotBottom, 4)

  if (series.minPrice == null || series.maxPrice == null) {
    return
  }

  const domain = padDomain(series.minPrice, series.maxPrice, 0.035)
  const priceSpan = Math.max(1, domain.high - domain.low)
  const x = (priceTicks) =>
    left + ((priceTicks - domain.low) / priceSpan) * plotWidth
  const y = (cumulative) =>
    plotBottom - (cumulative / series.maxCumulative) * plotHeight

  context.save()
  context.font = FONT
  context.fillStyle = MUTED
  context.textAlign = 'right'
  context.textBaseline = 'middle'
  for (let row = 0; row <= 4; row += 1) {
    const value = (series.maxCumulative * (4 - row)) / 4
    context.fillText(formatQty(value), left - 8, top + (plotHeight * row) / 4)
  }

  context.textAlign = 'center'
  context.textBaseline = 'top'
  context.fillText(formatPrice(market, domain.low), left, plotBottom + 9)
  context.fillText(formatPrice(market, domain.high), plotRight, plotBottom + 9)
  context.restore()

  const drawSide = (points, side) => {
    if (points.length === 0) return
    const ordered = side === 'bid' ? [...points].reverse() : points
    const first = ordered[0]
    const last = ordered[ordered.length - 1]
    const color = side === 'bid' ? GREEN : RED
    const fill =
      side === 'bid' ? 'rgba(67, 217, 139, 0.18)' : 'rgba(255, 107, 129, 0.16)'

    const path = new Path2D()
    path.moveTo(x(first.priceTicks), y(0))
    path.lineTo(x(first.priceTicks), y(first.cumulative))
    for (let index = 1; index < ordered.length; index += 1) {
      const previous = ordered[index - 1]
      const point = ordered[index]
      path.lineTo(x(point.priceTicks), y(previous.cumulative))
      path.lineTo(x(point.priceTicks), y(point.cumulative))
    }
    path.lineTo(x(last.priceTicks), y(0))
    path.closePath()

    context.save()
    context.fillStyle = fill
    context.fill(path)

    context.beginPath()
    context.moveTo(x(first.priceTicks), y(first.cumulative))
    for (let index = 1; index < ordered.length; index += 1) {
      const previous = ordered[index - 1]
      const point = ordered[index]
      context.lineTo(x(point.priceTicks), y(previous.cumulative))
      context.lineTo(x(point.priceTicks), y(point.cumulative))
    }
    context.strokeStyle = color
    context.lineWidth = 1.6
    context.lineJoin = 'round'
    context.stroke()
    context.restore()
  }

  drawSide(series.bids, 'bid')
  drawSide(series.asks, 'ask')

  if (series.bestBid != null && series.bestAsk != null) {
    const mid = (series.bestBid + series.bestAsk) / 2
    const midX = x(mid)
    context.save()
    context.setLineDash([3, 4])
    context.strokeStyle = 'rgba(179, 136, 255, 0.45)'
    context.beginPath()
    context.moveTo(midX, top)
    context.lineTo(midX, plotBottom)
    context.stroke()
    context.restore()
  }

  context.save()
  context.font = FONT
  context.textBaseline = 'top'
  context.fillStyle = GREEN
  context.textAlign = 'left'
  context.fillText('BID DEPTH', left, 1)
  context.fillStyle = RED
  context.textAlign = 'right'
  context.fillText('ASK DEPTH', plotRight, 1)
  context.restore()
}

export function DepthChart({ market, snapshot }) {
  const series = useMemo(
    () => buildDepthSeries(snapshot?.bids, snapshot?.asks),
    [snapshot],
  )
  const draw = useCallback(
    (canvas, width, height) => {
      drawDepthChart(canvas, width, height, series, market)
    },
    [series, market],
  )
  const { canvasRef, wrapRef } = useResponsiveCanvas(DEPTH_HEIGHT, draw)
  const empty = series.bids.length === 0 && series.asks.length === 0

  return (
    <section className="panel chart-panel">
      <ChartHeader
        title="Market depth"
        meta={`Book seq ${snapshot?.book_seq ?? '—'}`}
      />
      <div className="chart-wrap" ref={wrapRef}>
        <canvas
          ref={canvasRef}
          role="img"
          aria-label={
            empty
              ? 'Market depth chart with no resting levels'
              : `Market depth chart: ${formatQty(series.totalBidQty)} bid passes and ${formatQty(series.totalAskQty)} ask passes`
          }
        />
        {empty && (
          <div className="chart-empty">Waiting for book levels…</div>
        )}
      </div>
      <div className="chart-legend">
        <span>
          <i className="legend-dot bid" />
          Bids {formatQty(series.totalBidQty)}
        </span>
        <span>
          <i className="legend-dot ask" />
          Asks {formatQty(series.totalAskQty)}
        </span>
        <span className="chart-legend-note">Cumulative quantity by price</span>
      </div>
    </section>
  )
}

/**
 * @param {import('lightweight-charts').IChartApi} chart
 * @param {'area' | 'line' | 'candles' | 'bars'} chartType
 */
const createPriceSeries = (chart, chartType) => {
  const common = {
    priceLineVisible: false,
    lastValueVisible: true,
    priceFormat: { type: 'price', precision: 0, minMove: 1 },
  }

  if (chartType === 'candles') {
    return chart.addSeries(CandlestickSeries, {
      ...common,
      upColor: GREEN,
      downColor: RED,
      borderVisible: false,
      wickUpColor: GREEN,
      wickDownColor: RED,
    })
  }

  if (chartType === 'bars') {
    return chart.addSeries(BarSeries, {
      ...common,
      upColor: GREEN,
      downColor: RED,
      thinBars: false,
    })
  }

  if (chartType === 'line') {
    return chart.addSeries(LineSeries, {
      ...common,
      color: PURPLE,
      lineWidth: 2,
      crosshairMarkerBorderColor: '#0e0f16',
      crosshairMarkerBackgroundColor: PURPLE,
    })
  }

  return chart.addSeries(AreaSeries, {
    ...common,
    lineColor: PURPLE,
    lineWidth: 2,
    topColor: 'rgba(179, 136, 255, 0.30)',
    bottomColor: 'rgba(179, 136, 255, 0.015)',
    crosshairMarkerBorderColor: '#0e0f16',
    crosshairMarkerBackgroundColor: PURPLE,
  })
}

export function PriceChart({ market, trades }) {
  const [chartType, setChartType] = useState('area')
  const data = useMemo(
    () => buildTradeChartData(trades, 160, CANDLE_SEQUENCE_GROUP),
    [trades],
  )
  const containerRef = useRef(null)
  const chartRef = useRef(null)
  const seriesRef = useRef(null)
  const seriesTypeRef = useRef(null)
  const lastCandle = data.candles[data.candles.length - 1]
  const marketId = market?.id
  const isOhlc = chartType === 'candles' || chartType === 'bars'

  useEffect(() => {
    const container = containerRef.current
    if (!container) return undefined

    const chart = createChart(container, {
      autoSize: true,
      layout: {
        background: { type: ColorType.Solid, color: '#0e0f16' },
        textColor: MUTED,
        fontFamily:
          'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
        fontSize: 10,
        attributionLogo: false,
      },
      grid: {
        vertLines: { color: GRID },
        horzLines: { color: GRID },
      },
      rightPriceScale: {
        borderVisible: false,
        scaleMargins: { top: 0.1, bottom: 0.08 },
      },
      timeScale: {
        borderVisible: false,
        timeVisible: true,
        secondsVisible: false,
        rightOffset: 4,
        barSpacing: 9,
        minBarSpacing: 2,
        lockVisibleTimeRangeOnResize: true,
        tickMarkFormatter: (time) => {
          if (typeof time !== 'number') return ''
          return `#${Math.round(time)}`
        },
      },
      crosshair: {
        mode: CrosshairMode.Normal,
        vertLine: {
          color: 'rgba(179, 136, 255, 0.42)',
          width: 1,
          style: 2,
          labelBackgroundColor: '#222333',
        },
        horzLine: {
          color: 'rgba(179, 136, 255, 0.42)',
          width: 1,
          style: 2,
          labelBackgroundColor: '#222333',
        },
      },
      localization: {
        priceFormatter: (price) => formatPrice(market, price),
        timeFormatter: (time) =>
          typeof time === 'number' ? `seq ${Math.round(time)}` : '',
      },
      handleScroll: {
        mouseWheel: true,
        pressedMouseMove: true,
        horzTouchDrag: true,
        vertTouchDrag: false,
      },
      handleScale: {
        axisPressedMouseMove: true,
        mouseWheel: true,
        pinch: true,
      },
    })

    chartRef.current = chart
    return () => {
      seriesRef.current = null
      seriesTypeRef.current = null
      chartRef.current = null
      chart.remove()
    }
  }, [market])

  useEffect(() => {
    const chart = chartRef.current
    if (!chart) return

    const seriesData =
      chartType === 'candles' || chartType === 'bars'
        ? data.candles.map(({ time, open, high, low, close }) => ({
            time,
            open,
            high,
            low,
            close,
          }))
        : data.points

    if (seriesRef.current && seriesTypeRef.current === chartType) {
      seriesRef.current.setData(seriesData)
      chart.timeScale().scrollToRealTime()
      return
    }

    if (seriesRef.current) {
      chart.removeSeries(seriesRef.current)
      seriesRef.current = null
    }

    seriesRef.current = createPriceSeries(chart, chartType)
    seriesRef.current.setData(seriesData)
    seriesTypeRef.current = chartType

    if (seriesData.length > 0) {
      chart.timeScale().setVisibleLogicalRange({
        from: -1.5,
        to: seriesData.length + 3,
      })
    } else {
      chart.timeScale().fitContent()
    }
  }, [chartType, data, marketId])

  return (
    <section className="panel chart-panel price-chart-panel">
      <div className="panel-header chart-header">
        <div className="chart-heading">
          <h3>Price chart</h3>
          <span className="panel-meta">
            {data.tradeCount > 0
              ? `${data.tradeCount} prints · ${
                  isOhlc
                    ? `${data.candles.length} bars`
                    : `${data.sequenceCount} seq`
                }`
              : 'No prints'}
          </span>
        </div>
        <div
          className="chart-type-switch"
          role="group"
          aria-label="Price chart type"
        >
          {CHART_TYPES.map((type) => (
            <button
              key={type.id}
              type="button"
              className={`chart-type-button ${chartType === type.id ? 'active' : ''}`}
              aria-pressed={chartType === type.id}
              onClick={() => setChartType(type.id)}
            >
              {type.label}
            </button>
          ))}
        </div>
      </div>

      <div className="price-chart-wrap">
        <div
          className="price-chart-canvas"
          ref={containerRef}
          role="img"
          aria-label={
            lastCandle
              ? `Price chart in ${chartType} mode. Last trade ${formatPrice(market, lastCandle.close)}, sequence ${data.firstSeq} to ${data.lastSeq}`
              : 'Price chart with no trades yet'
          }
        />
        {!lastCandle && (
          <div className="chart-empty price-chart-empty">
            Waiting for the first print…
          </div>
        )}
      </div>

      <div className="chart-legend price-legend">
        <span>
          <i className="legend-dot price" />
          Last {lastCandle ? formatPrice(market, lastCandle.close) : '—'}
        </span>
        <span>
          Range{' '}
          {data.minPrice != null && data.maxPrice != null
            ? `${formatPrice(market, data.minPrice)} – ${formatPrice(market, data.maxPrice)}`
            : '—'}
        </span>
        <span className="chart-legend-note">
          {isOhlc
            ? `${CANDLE_SEQUENCE_GROUP} matching sequences per bar`
            : 'Streaming close by matching sequence'}
        </span>
        <a
          className="chart-attribution"
          href="https://www.tradingview.com/lightweight-charts/"
          target="_blank"
          rel="noreferrer"
        >
          Charts by TradingView
        </a>
      </div>
    </section>
  )
}

function ChartHeader({ title, meta }) {
  return (
    <div className="panel-header">
      <h3>{title}</h3>
      <span className="panel-meta">{meta}</span>
    </div>
  )
}
