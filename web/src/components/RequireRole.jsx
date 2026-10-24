import { Navigate, useLocation } from 'react-router-dom'
import { useAuthStore } from '../stores/useAuthStore'

// Wraps a page so only logged-in users (optionally only certain roles) can see it.
//   <RequireRole roles={['admin']}><AdminPage /></RequireRole>
// Leave `roles` out to allow any logged-in user.
export function RequireRole({ roles, children }) {
  const status = useAuthStore((state) => state.status)
  const user = useAuthStore((state) => state.user)
  const location = useLocation()

  if (status === 'loading') {
    return <div className="loading-panel">Checking your session…</div>
  }
  if (!user) {
    return <Navigate to="/login" replace state={{ from: location.pathname, roles }} />
  }
  if (roles && !roles.includes(user.role)) {
    return (
      <div className="loading-panel">
        <p>You do not have access to this page.</p>
      </div>
    )
  }
  return children
}