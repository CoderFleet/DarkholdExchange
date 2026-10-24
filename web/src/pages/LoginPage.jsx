import { useEffect } from 'react'
import { Link, Navigate, useLocation } from 'react-router-dom'
import { isMockAuth } from '../authApi'
import { AuthForm } from '../components/AuthForm'
import { roleHome, useAuthStore } from '../stores/useAuthStore'

export function LoginPage() {
  const user = useAuthStore((state) => state.user)
  const login = useAuthStore((state) => state.login)
  const submitting = useAuthStore((state) => state.submitting)
  const error = useAuthStore((state) => state.error)
  const clearError = useAuthStore((state) => state.clearError)
  const location = useLocation()

  // Start with a clean slate, so an old error from another page is not shown.
  useEffect(() => {
    clearError()
  }, [clearError])

  // Logged in (already, or just now): go where they were headed if their role may see it,
  // otherwise to their role's home page.
  if (user) {
    const { from, roles } = location.state ?? {}
    const allowed = from && (!roles || roles.includes(user.role))
    return <Navigate to={allowed ? from : roleHome(user.role)} replace />
  }

  return (
    <div className="auth-page">
      <AuthForm
        title="Log in"
        submitLabel="Log in"
        mode="login"
        onSubmit={login}
        submitting={submitting}
        error={error}
        footer={
          <>
            No account? <Link to="/signup">Sign up</Link>
          </>
        }
      />
      {isMockAuth && (
        <p className="auth-note">
          Demo mode, no server yet. Try admin / admin-pass-1, issuer / issuer-pass-1 or
          trader1 / trader-pass-1, or sign up.
        </p>
      )}
    </div>
  )
}