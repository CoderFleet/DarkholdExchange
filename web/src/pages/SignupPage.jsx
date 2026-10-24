import { useEffect } from 'react'
import { Link, Navigate } from 'react-router-dom'
import { AuthForm } from '../components/AuthForm'
import { roleHome, useAuthStore } from '../stores/useAuthStore'

export function SignupPage() {
  const user = useAuthStore((state) => state.user)
  const signup = useAuthStore((state) => state.signup)
  const submitting = useAuthStore((state) => state.submitting)
  const error = useAuthStore((state) => state.error)
  const clearError = useAuthStore((state) => state.clearError)

  useEffect(() => {
    clearError()
  }, [clearError])

  // Sign-up logs you in straight away, so once we know who you are, move on.
  if (user) return <Navigate to={roleHome(user.role)} replace />

  return (
    <div className="auth-page">
      <AuthForm
        title="Create an account"
        submitLabel="Sign up"
        mode="signup"
        onSubmit={signup}
        submitting={submitting}
        error={error}
        footer={
          <>
            Already have an account? <Link to="/login">Log in</Link>
          </>
        }
      />
      <p className="auth-note">
        Pick a username of 3-32 letters, numbers or underscores, and a password of 8-128
        characters.
      </p>
    </div>
  )
}