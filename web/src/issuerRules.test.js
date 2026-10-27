import { describe, expect, it } from 'vitest'
import { qtyProblem } from './issuerRules'

describe('qtyProblem', () => {
  it('accepts whole numbers from 1 to 1000', () => {
    for (const ok of ['1', '25', ' 40 ', '1000']) expect(qtyProblem(ok)).toBeNull()
  })

  it('rejects everything else', () => {
    for (const bad of ['', '0', '1001', '-3', '2.5', '1e3', 'ten', null, undefined]) {
      expect(qtyProblem(bad)).not.toBeNull()
    }
  })
})