// @ts-check
/// <reference lib="webworker" />

/**
 * Canonical per-user private feed (row 7).
 *
 * One worker connection follows one `/ws` socket authenticated with the login
 * token. The worker owns the socket, keeps MessagePack decoding off the React
 * thread, reconnects with bounded backoff, and forwards only the private
 * frames — `Balances`, `FillSelf`, `EscrowUpdate` — to the wallet store. Public
 * market data never reaches the main thread from here.
 */

import { decodeServerMessage, encodeClientMessage, subscribe } from '../protocol/index.js'
import { isPrivateFrame } from './walletFeed.js'

const RECONNECT_BASE_MS = 400
const RECONNECT_MAX_MS = 5_000

/** @type {WebSocket | null} */
let socket = null
/** @type {ReturnType<typeof setTimeout> | null} */
let reconnectTimer = null
let reconnectAttempt = 0
let shouldReconnect = false
let session = 0

/** @type {{ userId: number, token: string | null }} */
let connection = { userId: 0, token: null }

const emitStatus = (status) => {
  self.postMessage({ type: 'status', status })
}

const emitError = (message) => {
  self.postMessage({ type: 'error', message })
}

/** @param {import('../protocol/generated/WsServerMessage').WsServerMessage} message */
const emitMessage = (message) => {
  self.postMessage({ type: 'message', message })
}

const websocketUrl = () => {
  const protocol = self.location.protocol === 'https:' ? 'wss' : 'ws'
  return `${protocol}://${self.location.host}/ws?token=${encodeURIComponent(connection.token ?? '')}`
}

const sendSubscription = () => {
  if (!socket || socket.readyState !== WebSocket.OPEN) return
  // The all-markets socket baseline already covers public data after the
  // upgrade. This wallet feed only wants the user's own ledger events, so pin
  // the public half to a market id that does not exist: its baseline is empty
  // and later market frames are filtered server-side, while the private
  // frames bypass that filter entirely.
  socket.send(encodeClientMessage(subscribe({ marketIds: [0], depth: 1 })))
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
    sendSubscription()
  }

  nextSocket.onmessage = (event) => {
    if (id !== session) return
    try {
      const message = decodeServerMessage(event.data)
      // The server only targets private frames at this socket's token-checked
      // user, and lagged streams are re-baselined server-side.
      if (isPrivateFrame(message)) emitMessage(message)
    } catch {
      emitError('Malformed private-data frame')
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
    typeof options.token !== 'string' ||
    !options.token
  ) {
    return
  }

  shouldReconnect = false
  session += 1
  clearReconnectTimer()
  closeSocket()

  connection = {
    userId: Number(options.userId),
    token: options.token,
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
    default:
      break
  }
}
