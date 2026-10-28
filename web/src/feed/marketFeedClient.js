// @ts-check
/**
 * Thin main-thread wrapper around the market-feed worker.
 *
 * Keeping the Worker API in one file means React components never touch raw
 * sockets, MessagePack frames, or worker message plumbing.
 */

/** @typedef {import('../protocol/generated/WsServerMessage').WsServerMessage} WsServerMessage */
/** @typedef {'idle' | 'connecting' | 'live' | 'reconnecting' | 'offline'} FeedStatus */

/**
 * @typedef {{
 *   onStatus: (status: FeedStatus) => void,
 *   onMessage: (message: WsServerMessage, marketId: number) => void,
 *   onError: (message: string) => void,
 * }} FeedClientHandlers
 */

/**
 * @typedef {{
 *   userId: number,
 *   marketId: number,
 *   depth: number,
 *   token?: string | null,
 * }} FeedConnection
 */

/**
 * @param {FeedClientHandlers} handlers
 */
export const createMarketFeedClient = ({ onStatus, onMessage, onError }) => {
  const worker = new Worker(new URL('./marketFeedWorker.js', import.meta.url), {
    type: 'module',
  })

  worker.addEventListener('message', (event) => {
    const payload = /** @type {any} */ (event.data)
    if (!payload || typeof payload !== 'object') return

    switch (payload.type) {
      case 'status':
        onStatus(payload.status)
        break
      case 'message':
        onMessage(payload.message, payload.marketId)
        break
      case 'error':
        onError(payload.message)
        break
      default:
        break
    }
  })

  return {
    /** @param {FeedConnection} connection */
    connect: ({ userId, marketId, depth, token }) => {
      worker.postMessage({ type: 'connect', userId, marketId, depth, token })
    },
    resync: () => {
      worker.postMessage({ type: 'resync' })
    },
    disconnect: () => {
      worker.postMessage({ type: 'disconnect' })
    },
    dispose: () => {
      worker.terminate()
    },
  }
}
