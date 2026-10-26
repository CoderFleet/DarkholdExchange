// The same sign-up rules the server checks (crates/auth). The server is the real judge;
// these let the form say what is wrong before anything is sent.

export const USERNAME_MIN = 3
export const USERNAME_MAX = 32
export const PASSWORD_MIN = 8
export const PASSWORD_MAX = 128

// Returns a sentence saying what is wrong, or null when the username is fine.
export function usernameProblem(raw) {
  // The server trims and lowercases the name, so we judge it the same way.
  const name = String(raw ?? '').trim().toLowerCase()
  if (name.length < USERNAME_MIN || name.length > USERNAME_MAX) {
    return `Username must be ${USERNAME_MIN}-${USERNAME_MAX} characters.`
  }
  if (!/^[a-z0-9_]+$/.test(name)) {
    return 'Username can only use letters, numbers and underscores.'
  }
  return null
}

// Returns a sentence saying what is wrong, or null when the password is fine.
export function passwordProblem(password) {
  // Spread into an array so an emoji counts as one character, like the server counts.
  const length = [...String(password ?? '')].length
  if (length < PASSWORD_MIN || length > PASSWORD_MAX) {
    return `Password must be ${PASSWORD_MIN}-${PASSWORD_MAX} characters.`
  }
  return null
}