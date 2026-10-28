// @ts-check
// The proof-of-work puzzle (docs/AUTH.md section 9): find a number `nonce` so that the SHA-256
// hash of "<challenge>~<nonce>" starts with at least `bits` zero bits. The server (crates/pow)
// checks the same thing, so both sides must hash exactly the same text.
import { sha256 } from './sha256.js'

/**
 * How many bits at the start of `hash` are zero.
 * @param {Uint8Array} hash
 */
export const leadingZeroBits = (hash) => {
  let bits = 0
  for (const byte of hash) {
    if (byte === 0) {
      bits += 8
    } else {
      return bits + Math.clz32(byte) - 24
    }
  }
  return bits
}

/**
 * Tries 0, 1, 2, ... and returns the first nonce that solves the puzzle. Takes about 2^bits tries.
 * @param {string} challenge
 * @param {number} bits
 * @returns {number}
 */
export const solve = (challenge, bits) => {
  const prefix = new TextEncoder().encode(`${challenge}~`)
  for (let nonce = 0; ; nonce += 1) {
    const digits = String(nonce)
    const message = new Uint8Array(prefix.length + digits.length)
    message.set(prefix)
    for (let i = 0; i < digits.length; i += 1) message[prefix.length + i] = digits.charCodeAt(i)
    if (leadingZeroBits(sha256(message)) >= bits) return nonce
  }
}