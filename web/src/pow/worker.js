// @ts-check
// Runs in a Web Worker, so solving a puzzle does not freeze the page.
import { solve } from './solve.js'

self.onmessage = (/** @type {MessageEvent<{ challenge: string, bits: number }>} */ event) => {
  const { challenge, bits } = event.data
  self.postMessage({ nonce: solve(challenge, bits) })
}