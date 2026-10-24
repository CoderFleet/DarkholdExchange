import { useEffect } from 'react'
import { Link, Navigate, Route, Routes } from 'react-router-dom'
import App from '../App'
import { AdminPage } from '../pages/AdminPage'
import { IssuerPage } from '../pages/IssuerPage'
import { LoginPage } from '../pages/LoginPage'
import { SignupPage } from '../pages/SignupPage'
import { useAuthStore } from '../stores/useAuthStore'
import { Navbar } from './Navbar'
import { RequireRole } from './RequireRole'
import '../shell.css'

function NotFound() {
  return (
    <div className="loading-panel">
      <p>Page not found.</p>
      <Link to="/market">Go to the market</Link>
    </div>
  )
}

// The app shell: a navbar on top, and below it exactly one page chosen by the URL.
// Other pages are added here as new <Route> lines.
export function AppShell() {
  const restore = useAuthStore((state) => state.restore)

  // When the app opens, check whether a saved login is still valid.
  useEffect(() => {
    restore()
  }, [restore])

  return (
    <>
      <Navbar />
      <Routes>
        <Route path="/" element={<Navigate to="/market" replace />} />
        <Route path="/market" element={<App />} />
        <Route path="/login" element={<LoginPage />} />
        <Route path="/signup" element={<SignupPage />} />
        <Route
          path="/admin"
          element={
            <RequireRole roles={['admin']}>
              <AdminPage />
            </RequireRole>
          }
        />
        <Route
          path="/issuer"
          element={
            <RequireRole roles={['issuer']}>
              <IssuerPage />
            </RequireRole>
          }
        />
        <Route path="*" element={<NotFound />} />
      </Routes>
    </>
  )
}