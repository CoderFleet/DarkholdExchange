// @ts-check
/**
 * Thin main-thread wrapper around the per-user private feed worker.
 *
 * Keeping the Worker API in one file means React components never touch raw
 * sockets, MessagePack frames, or worker message plumbing.
 */

/** @typedef {import('../protocol/generated/WsServerMessage').WsServerMessage} WsServerMessage */
/** @typedef {'idle' | 'connecting' | 'live' | 'reconnecting' | 'offline'} PrivateFeedStatus */

/**
 * @typedef {{
 *   onStatus: (status: PrivateFeedStatus) => void,
 *   onMessage: (message: WsServerMessage) => void,
 *   onError: (message: string) => void,
 * }} WalletFeedClientHandlers
 */

/**
 * @typedef {{
 *   userId: number,
 *   token?: string | null,
 * }} WalletFeedConnection
 */

/**
 * @param {WalletFeedClientHandlers} handlers
 */
export const createWalletFeedClient = ({ onStatus, onMessage, onError }) => {
  const worker = new Worker(new URL('./walletFeedWorker.js', import.meta.url), {
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
        onMessage(payload.message)
        break
      case 'error':
        onError(payload.message)
        break
      default:
        break
    }
  })

  return {
    /** @param {WalletFeedConnection} connection */
    connect: ({ userId, token }) => {
      worker.postMessage({ type: 'connect', userId, token })
    },
    disconnect: () => {
      worker.postMessage({ type: 'disconnect' })
    },
    dispose: () => {
      worker.terminate()
    },
  }
}
