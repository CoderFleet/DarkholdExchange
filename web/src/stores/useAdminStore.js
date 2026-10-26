import { create } from 'zustand'
import { adminErrorText, createEvent, createTier, getAdminEvents, publishTier } from '../adminApi'

export const useAdminStore = create((set) => {
  // Runs one admin action. On success the list is reloaded, so the page always shows what the
  // server really has. Returns true or false so a form knows whether to clear itself.
  const run = async (action) => {
    set({ busy: true, error: null })
    try {
      await action()
      set({ events: await getAdminEvents(), busy: false })
      return true
    } catch (err) {
      set({ error: adminErrorText(err), busy: false })
      return false
    }
  }

  return {
    events: [],
    // 'loading' until the first list arrives, then 'ready'.
    status: 'loading',
    error: null,
    busy: false,

    load: async () => {
      try {
        set({ events: await getAdminEvents(), status: 'ready', error: null })
      } catch (err) {
        set({ status: 'ready', error: adminErrorText(err) })
      }
    },

    addEvent: (name, venue) => run(() => createEvent(name, venue || undefined)),
    addTier: (eventId, name, faceValueMinor) =>
      run(() => createTier(eventId, { name, face_value_minor: faceValueMinor })),
    publish: (marketId) => run(() => publishTier(marketId)),
    clearError: () => set({ error: null }),
  }
})