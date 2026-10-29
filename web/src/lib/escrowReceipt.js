// Seller-side delivery receipts for escrow markets.
//
// The signed bytes must match dx_types::DeliveryReceipt::canonical_bytes exactly:
// the domain string, then six unsigned big-endian u64 values.

const DOMAIN = new TextEncoder().encode('darkhold-escrow-receipt-v1')
/** @param {number} userId */
const keyStorage = (userId) => `darkhold.escrow.sellerKey.${userId}`

/** @param {Uint8Array} bytes */
export const bytesToHex = (bytes) =>
  Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('')

/**
 * @param {{
 *   trade_id: number,
 *   market_id: number,
 *   qty: number,
 *   buyer_id: number,
 *   seller_id: number,
 *   delivered_at_ms: number,
 * }} receipt
 * @returns {Uint8Array}
 */
export const receiptMessage = (receipt) => {
  const values = [
    receipt.trade_id,
    receipt.market_id,
    receipt.qty,
    receipt.buyer_id,
    receipt.seller_id,
    receipt.delivered_at_ms,
  ]
  const bytes = new Uint8Array(DOMAIN.length + values.length * 8)
  bytes.set(DOMAIN, 0)
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  let offset = DOMAIN.length
  for (const value of values) {
    view.setBigUint64(offset, BigInt(value), false)
    offset += 8
  }
  return bytes
}

const subtle = () => {
  const value = globalThis.crypto?.subtle
  if (!value) {
    throw new Error('This browser does not expose Web Crypto.')
  }
  return value
}

/**
 * Generate an ed25519 key pair, keep the private key in localStorage for this
 * demo, and return the raw public key as hex.
 *
 * @param {number} userId
 * @returns {Promise<string>}
 */
export const generateSellerKey = async (userId) => {
  const crypto = subtle()
  let pair
  try {
    pair = await crypto.generateKey({ name: 'Ed25519' }, true, ['sign', 'verify'])
  } catch {
    throw new Error('This browser does not support Ed25519 signing.')
  }
  const publicKey = new Uint8Array(await crypto.exportKey('raw', pair.publicKey))
  const privateKey = await crypto.exportKey('jwk', pair.privateKey)
  localStorage.setItem(keyStorage(userId), JSON.stringify(privateKey))
  return bytesToHex(publicKey)
}

/**
 * @param {number} userId
 * @returns {boolean}
 */
export const hasSellerKey = (userId) => Boolean(localStorage.getItem(keyStorage(userId)))

/**
 * Sign a delivery receipt with the browser-held seller key.
 *
 * @param {Parameters<typeof receiptMessage>[0]} receipt
 * @param {number} userId
 * @returns {Promise<string>} signature as hex
 */
export const signDeliveryReceipt = async (receipt, userId) => {
  const crypto = subtle()
  const stored = localStorage.getItem(keyStorage(userId))
  if (!stored) {
    throw new Error('Register your delivery signing key before marking a trade delivered.')
  }
  let privateKey
  try {
    privateKey = await crypto.importKey(
      'jwk',
      JSON.parse(stored),
      { name: 'Ed25519' },
      false,
      ['sign'],
    )
  } catch {
    throw new Error('The stored delivery signing key is invalid. Generate a new one.')
  }
  const receiptBytes = receiptMessage(receipt)
  // crypto.sign wants a transferable plain-ArrayBuffer view (not a
  // SharedArrayBuffer-backed one), so copy the canonical bytes once.
  const signedBytes = /** @type {Uint8Array<ArrayBuffer>} */ (
    new Uint8Array(receiptBytes.byteLength)
  )
  signedBytes.set(receiptBytes)
  const signature = await crypto.sign({ name: 'Ed25519' }, privateKey, signedBytes)
  return bytesToHex(new Uint8Array(signature))
}
