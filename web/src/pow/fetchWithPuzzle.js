// @ts-check
// A fetch that deals with "428 pow_required" (docs/AUTH.md section 9): when the server asks for a
// puzzle, get one from /api/pow/challenge, solve it in a Web Worker, and send the request again
// with the answer in the x-pow header. It asks at most once, so it can never loop.
import { solve } from './solve.js'

/**
 * Solves in a worker when the browser has them, otherwise on the page itself.
 * @param {string} challenge
 * @param {number} bits
 * @returns {Promise<number>}
 */
export const solveInBackground = (challenge, bits) => {
  if (typeof Worker === 'undefined') return Promise.resolve(solve(challenge, bits))
  return new Promise((resolve, reject) => {
    const worker = new Worker(new URL('./worker.js', import.meta.url), { type: 'module' })
    worker.onmessage = (event) => {
      worker.terminate()
      resolve(event.data.nonce)
    }
    worker.onerror = (event) => {
      worker.terminate()
      reject(new Error(event.message || 'puzzle solver failed'))
    }
    worker.postMessage({ challenge, bits })
  })
}

/**
 * @param {string} path
 * @param {RequestInit} [options]
 * @returns {Promise<Response>}
 */
export const fetchWithPuzzle = async (path, options) => {
  const response = await fetch(path, options)
  if (response.status !== 428) return response

  // Look at the body through a copy, so the caller can still read the original.
  const copy = typeof response.clone === 'function' ? response.clone() : response
  const body = await copy.json().catch(() => null)
  if (body?.reason !== 'pow_required') return response

  const offer = await fetch('/api/pow/challenge')
  if (!offer.ok) return response
  const { challenge, bits } = await offer.json()
  const nonce = await solveInBackground(challenge, bits)
  /** @type {Record<string, string>} */
  const headers = { ...(/** @type {Record<string, string> | undefined} */ (options?.headers)) }
  headers['x-pow'] = `${challenge}~${nonce}`
  return fetch(path, { ...options, headers })
}