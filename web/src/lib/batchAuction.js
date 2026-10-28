/**
 * Compute the 32-byte SHA-256 commitment hex string for a batch bid.
 *
 * Pre-image layout:
 * - price_ticks as 8 little-endian bytes
 * - qty as 8 little-endian bytes
 * - salt as 32 raw bytes
 * Total: 48 bytes
 *
 * @param {number} priceTicks
 * @param {number} qty
 * @param {Uint8Array} salt32
 * @returns {Promise<{ commitmentHex: string, saltHex: string }>}
 */
export async function computeCommitment(priceTicks, qty, salt32) {
  const buf = new Uint8Array(48)
  const view = new DataView(buf.buffer)
  view.setBigUint64(0, BigInt(priceTicks), true)
  view.setBigUint64(8, BigInt(qty), true)
  buf.set(salt32, 16)

  const hashBuffer = await crypto.subtle.digest('SHA-256', buf)
  const hashArray = Array.from(new Uint8Array(hashBuffer))
  const commitmentHex = hashArray.map((b) => b.toString(16).padStart(2, '0')).join('')
  const saltHex = Array.from(salt32).map((b) => b.toString(16).padStart(2, '0')).join('')

  return { commitmentHex, saltHex }
}

/**
 * Generate 32 secure random bytes for a batch bid salt.
 * @returns {Uint8Array}
 */
export function generateSalt() {
  const salt = new Uint8Array(32)
  crypto.getRandomValues(salt)
  return salt
}
