// @ts-check
// Holds the current login token so api.js can attach it to requests.
// It is its own tiny file so api.js does not have to import the whole auth store.

/** @type {string | null} */
let token = null

/** @param {string | null} value */
export const setAuthToken = (value) => {
  token = value
}

/** @returns {string | null} */
export const getAuthToken = () => token

/** @type {(() => void) | null} */
let unauthorizedHandler = null

/**
 * Registers what to do when the server rejects the token we sent (expired, or the server
 * restarted with a new secret). The auth store uses it to log the user out.
 *
 * @param {(() => void) | null} handler
 */
export const setUnauthorizedHandler = (handler) => {
  unauthorizedHandler = handler
}

/** Called by api.js when a request that carried a token comes back 401 "unauthorized". */
export const notifyUnauthorized = () => {
  unauthorizedHandler?.()
}