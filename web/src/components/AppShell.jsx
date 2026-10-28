import { useEffect } from 'react'
import { Link, Navigate, Route, Routes, useParams } from 'react-router-dom'
import App from '../App'
import { AdminPage } from '../pages/AdminPage'
import { AuctionsPage } from '../pages/AuctionsPage'
import { AdminDisputesPage } from '../pages/AdminDisputesPage'
import { IssuerPage } from '../pages/IssuerPage'
import { LandingPage } from '../pages/LandingPage'
import { LoginPage } from '../pages/LoginPage'
import { MarketsPage } from '../pages/MarketsPage'
import { SignupPage } from '../pages/SignupPage'
import { TradeEscrowPage } from '../pages/TradeEscrowPage'
import { VerifyPage } from '../pages/VerifyPage'
import { useAuthStore } from '../stores/useAuthStore'
import { Navbar } from './Navbar'
import { RequireRole } from './RequireRole'
import { ShellFooter } from './ShellFooter'
import '../shell.css'

function TradeEscrowRouteWrapper() {
  const { tradeId } = useParams()
  return <TradeEscrowPage tradeId={tradeId} />
}

function NotFound() {
  return (
    <div className="loading-panel">
      <p>Page not found.</p>
      <Link to="/markets">Browse the markets</Link>
    </div>
  )
}

// The app shell: a navbar on top, exactly one page chosen by the URL, and a
// shared exchange footer. Market pages are deep-linkable, so a trader can keep
// several books open or share a link to one.
export function AppShell() {
  const restore = useAuthStore((state) => state.restore)

  // When the app opens, check whether a saved login is still valid.
  useEffect(() => {
    restore()
  }, [restore])

  return (
    <div className="shell-root">
      <Navbar />
      <div className="shell-main">
        <Routes>
          <Route path="/" element={<LandingPage />} />
          <Route path="/markets" element={<MarketsPage />} />
          <Route
            path="/markets/:marketId"
            element={
              <RequireRole>
                <App initialView="trade" />
              </RequireRole>
            }
          />
          <Route path="/market" element={<Navigate to="/markets" replace />} />
          <Route
            path="/wallet"
            element={
              <RequireRole>
                <App initialView="wallet" />
              </RequireRole>
            }
          />
          <Route
            path="/history"
            element={
              <RequireRole>
                <App initialView="history" />
              </RequireRole>
            }
          />
          <Route path="/auctions" element={<AuctionsPage />} />
          <Route path="/speed" element={<App initialView="speed" />} />
          <Route path="/verify" element={<VerifyPage />} />
          <Route path="/login" element={<LoginPage />} />
          <Route path="/signup" element={<SignupPage />} />
          <Route
            path="/trade/:tradeId"
            element={
              <RequireRole>
                <TradeEscrowRouteWrapper />
              </RequireRole>
            }
          />
          <Route
            path="/admin/disputes"
            element={
              <RequireRole roles={['admin']}>
                <AdminDisputesPage />
              </RequireRole>
            }
          />
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
      </div>
      <ShellFooter />
    </div>
  )
}
