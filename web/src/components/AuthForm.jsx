import { useState } from 'react'

// One form shared by the login and signup pages. It only collects the username and
// password; what happens on submit is decided by the page that uses it.
export function AuthForm({ title, submitLabel, mode, onSubmit, submitting, error, footer }) {
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')

  const handleSubmit = (event) => {
    // Stop the browser's default "reload the page" behaviour for forms.
    event.preventDefault()
    onSubmit(username.trim(), password)
  }

  return (
    <form className="panel auth-card" onSubmit={handleSubmit}>
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
          autoComplete="username"
          required
        />
      </label>
      <label className="field">
        <span>Password</span>
        <input
          type="password"
          value={password}
          onChange={(event) => setPassword(event.target.value)}
          autoComplete={mode === 'signup' ? 'new-password' : 'current-password'}
          required
        />
      </label>
      <button type="submit" className="auth-button" disabled={submitting}>
        {submitting ? 'Please wait…' : submitLabel}
      </button>
      <p className="auth-footer">{footer}</p>
    </form>
  )
}