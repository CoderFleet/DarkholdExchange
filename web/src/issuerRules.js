// The same limits the server checks (docs/AUTH.md section 6.1). The server is the real judge;
// these let the form say what is wrong before anything is sent.
import { MAX_ISSUE_QTY } from './issuerApi'

// Returns a sentence saying what is wrong with the quantity, or null when it is fine.
export function qtyProblem(raw) {
  const text = String(raw ?? '').trim()
  if (!/^\d+$/.test(text)) return `Enter a whole number from 1 to ${MAX_ISSUE_QTY}.`
  const qty = Number(text)
  if (qty < 1 || qty > MAX_ISSUE_QTY) return `Enter a whole number from 1 to ${MAX_ISSUE_QTY}.`
  return null
}