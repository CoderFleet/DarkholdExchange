// Turns what an admin types ("5000" or "5000.50") into paise, using whole numbers only.
// Floats are avoided on purpose: 0.1 + 0.2 is not 0.3 in JavaScript, and money must be exact.
const RUPEES = /^(\d{1,10})(?:\.(\d{1,2}))?$/

/**
 * @param {string} text
 * @returns {number | null} paise, or null if the text is not a positive amount
 */
export const rupeesToMinor = (text) => {
  const match = RUPEES.exec(String(text).trim())
  if (!match) return null
  const minor = Number(match[1]) * 100 + Number((match[2] ?? '').padEnd(2, '0'))
  return minor > 0 ? minor : null
}