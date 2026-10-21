const jsonHeaders = (userId) => ({
  'content-type': 'application/json',
  'x-dev-user': String(userId),
})

const request = async (path, options = {}) => {
  const response = await fetch(path, options)
  const body = await response.json().catch(() => null)
  if (!response.ok) {
    const error = new Error(body?.reason ? String(body.reason).replaceAll('_', ' ') : `HTTP ${response.status}`)
    error.reason = body?.reason
    throw error
  }
  return body
}

export const getMarkets = () => request('/api/markets')

export const getBalances = (userId) => request(`/api/users/${userId}/balances`)

export const getOpenOrders = (userId) => request(`/api/users/${userId}/orders`)

export const getTrades = (marketId, limit = 40) =>
  request(`/api/markets/${marketId}/trades?limit=${limit}`)

export const postCommand = (command, userId) =>
  request('/api/commands', {
    method: 'POST',
    headers: jsonHeaders(userId),
    body: JSON.stringify(command),
  })

export const websocketUrl = (userId) => {
  const protocol = window.location.protocol === 'https:' ? 'wss' : 'ws'
  return `${protocol}://${window.location.host}/ws?user_id=${userId}`
}
