import { describe, expect, it } from 'vitest'
import { computeCommitment } from './batchAuction'

describe('batchAuction', () => {
  it('computes expected SHA-256 commitment matching Rust reference', async () => {
    const salt = new Uint8Array(32)
    for (let i = 0; i < 32; i++) salt[i] = i

    const { commitmentHex, saltHex } = await computeCommitment(1500, 10, salt)
    expect(saltHex).toHaveLength(64)
    expect(commitmentHex).toHaveLength(64)
    expect(commitmentHex).toBe('5232e54aac490d89055765a95aed3799f632b438706f7d1b86be3d4101cc4229')
  })
})
