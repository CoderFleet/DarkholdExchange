// @ts-check
/// <reference lib="webworker" />

/**
 * Canonical T1 market feed.
 *
 * One worker connection follows one `/ws/market/{id}` feed. The worker owns the
 * socket, keeps MessagePack decoding off the React thread, reconnects with
 * bounded backoff, and forwards decoded frames to the market store.
 */

import {
  decodeServerMessage,
  encodeClientMessage,
  subscribe,
} from '../protocol/index.js'

const DEFAULT_DEPTH = 50
const RECONNECT_BASE_MS = 400
const RECONNECT_MAX_MS = 5_000

/** @type {WebSocket | null} */
let socket = null
/** @type {ReturnType<typeof setTimeout> | null} */
let reconnectTimer = null
let reconnectAttempt = 0
let shouldReconnect = false
let session = 0

/** @type {{ userId: number, marketId: number, depth: number, token: string | null }} */
let connection = { userId: 1, marketId: 1, depth: DEFAULT_DEPTH, token: null }

const emitStatus = (status) => {
  self.postMessage({ type: 'status', status })
}

const emitError = (message) => {
  self.postMessage({ type: 'error', message })
}

const emitMessage = (message) => {
  self.postMessage({
    type: 'message',
    marketId: connection.marketId,
    message,
  })
}

const websocketUrl = () => {
  const protocol = self.location.protocol === 'https:' ? 'wss' : 'ws'
  // A logged-in user proves who they are with the login token. Without one, fall back to the
  // plain id, which the server believes only in dev mode (docs/AUTH.md section 2.5).
  const who = connection.token
    ? `token=${encodeURIComponent(connection.token)}`
    : `user_id=${connection.userId}`
  return `${protocol}://${self.location.host}/ws/market/${connection.marketId}?${who}`
}

const sendSubscription = () => {
  if (!socket || socket.readyState !== WebSocket.OPEN) return
  socket.send(
    encodeClientMessage(
      subscribe({
        marketIds: [connection.marketId],
        depth: connection.depth,
      }),
    ),
  )
}

const clearReconnectTimer = () => {
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer)
    reconnectTimer = null
  }
}

const start = () => {
  if (!shouldReconnect) return

  const id = ++session
  emitStatus(reconnectAttempt === 0 ? 'connecting' : 'reconnecting')

  const nextSocket = new WebSocket(websocketUrl())
  socket = nextSocket
  nextSocket.binaryType = 'arraybuffer'

  nextSocket.onopen = () => {
    if (id !== session) return
    reconnectAttempt = 0
    emitStatus('live')
    // The fixed-market endpoint already sends a baseline on upgrade. This
    // subscription applies the requested depth and is reused for client-side
    // resync requests later.
    sendSubscription()
  }

  nextSocket.onmessage = (event) => {
    if (id !== session) return
    try {
      emitMessage(decodeServerMessage(event.data))
    } catch {
      emitError('Malformed market-data frame')
    }
  }

  nextSocket.onerror = () => {
    if (id === session) nextSocket.close()
  }

  nextSocket.onclose = () => {
    if (id !== session) return
    socket = null
    if (!shouldReconnect) {
      emitStatus('offline')
      return
    }

    emitStatus('reconnecting')
    const delay = Math.min(
      RECONNECT_MAX_MS,
      RECONNECT_BASE_MS * 2 ** Math.min(reconnectAttempt, 4),
    )
    reconnectAttempt += 1
    reconnectTimer = setTimeout(start, delay)
  }
}

const closeSocket = () => {
  const previous = socket
  socket = null
  if (!previous) return
  previous.onclose = null
  previous.close()
}

const connect = (options) => {
  if (
    !options ||
    !Number.isFinite(options.userId) ||
    !Number.isFinite(options.marketId)
  ) {
    return
  }

  shouldReconnect = false
  session += 1
  clearReconnectTimer()
  closeSocket()

  connection = {
    userId: Number(options.userId),
    marketId: Number(options.marketId),
    depth: Number.isFinite(options.depth)
      ? Math.max(1, Math.min(200, Number(options.depth)))
      : DEFAULT_DEPTH,
      token: typeof options.token === 'string' && options.token ? options.token : null,
  }
  reconnectAttempt = 0
  shouldReconnect = true
  start()
}

const disconnect = () => {
  shouldReconnect = false
  session += 1
  clearReconnectTimer()
  closeSocket()
  emitStatus('offline')
}

const resync = () => {
  sendSubscription()
}

self.onmessage = (event) => {
  const payload = /** @type {any} */ (event.data)
  if (!payload || typeof payload !== 'object') return

  switch (payload.type) {
    case 'connect':
      connect(payload)
      break
    case 'disconnect':
      disconnect()
      break
    case 'resync':
      resync()
      break
    default:
      break
  }
}
