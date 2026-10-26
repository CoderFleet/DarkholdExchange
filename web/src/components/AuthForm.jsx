import { useId, useState } from 'react'
import { passwordProblem, usernameProblem } from '../authRules'

// One form shared by the login and signup pages. It only collects the username and
// password; what happens on submit is decided by the page that uses it.
export function AuthForm({ title, submitLabel, mode, onSubmit, submitting, error, footer }) {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [showPassword, setShowPassword] = useState(false)
  // A problem is only shown once the person has left that box (or tried to submit).
  const [touched, setTouched] = useState({ username: false, password: false })
  const usernameHintId = useId()
  const passwordHintId = useId()

  // Only sign-up checks the rules. Logging in must accept whatever the account already has.
  const isSignup = mode === 'signup'
  const usernameError = isSignup && touched.username ? usernameProblem(username) : null
  const passwordError = isSignup && touched.password ? passwordProblem(password) : null

  const handleSubmit = (event) => {
    // Stop the browser's default "reload the page" behaviour for forms.
    event.preventDefault()
    if (isSignup && (usernameProblem(username) || passwordProblem(password))) {
      setTouched({ username: true, password: true })
      return
    }
    onSubmit(username.trim(), password)
  }

  return (
    <form className="panel auth-card" onSubmit={handleSubmit} noValidate>
      <h2>{title}</h2>
      {error && (
        <div className="banner error" role="alert">
          {error}
        </div>
      )}
      <label className="field">
        <span>Username</span>
        <input
          value={username}
          onChange={(event) => setUsername(event.target.value)}
          onBlur={() => setTouched((current) => ({ ...current, username: true }))}
          autoComplete="username"
          autoCapitalize="none"
          spellCheck={false}
          aria-invalid={usernameError ? true : undefined}
          aria-describedby={usernameError ? usernameHintId : undefined}
          required
        />
      </label>
      {usernameError && (
        <p className="field-error" id={usernameHintId}>
          {usernameError}
        </p>
      )}
      <div className="password-field">
        <label className="field">
          <span>Password</span>
          <input
            type={showPassword ? 'text' : 'password'}
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            onBlur={() => setTouched((current) => ({ ...current, password: true }))}
            autoComplete={isSignup ? 'new-password' : 'current-password'}
            aria-invalid={passwordError ? true : undefined}
            aria-describedby={passwordError ? passwordHintId : undefined}
            required
          />
        </label>
        <button
          type="button"
          className="password-toggle"
          onClick={() => setShowPassword((current) => !current)}
          aria-label={showPassword ? 'Hide password' : 'Show password'}
          aria-pressed={showPassword}
        >
          <EyeIcon crossedOut={showPassword} />
        </button>
      </div>
      {passwordError && (
        <p className="field-error" id={passwordHintId}>
          {passwordError}
        </p>
      )}
      <button type="submit" className="auth-button" disabled={submitting}>
        {submitting ? 'Please wait…' : submitLabel}
      </button>
      <p className="auth-footer">{footer}</p>
    </form>
  )
}

// A small eye. With a line through it, it means "hide the password again".
function EyeIcon({ crossedOut }) {
  return (
    <svg
      width="20"
      height="20"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M1 12s4-7 11-7 11 7 11 7-4 7-11 7S1 12 1 12z" />
      <circle cx="12" cy="12" r="3" />
      {crossedOut && <path d="M3 3l18 18" />}
    </svg>
  )
}