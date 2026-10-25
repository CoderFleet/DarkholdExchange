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