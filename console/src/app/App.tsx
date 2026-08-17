import { NavLink, Route, Routes } from 'react-router-dom'

import { CallDetailPage } from '../features/calls/CallDetailPage'
import { CallsPage } from '../features/calls/CallsPage'
import { ModelsPage } from '../features/models/ModelsPage'
import { ProvidersPage } from '../features/providers/ProvidersPage'
import { ObservabilityPage } from '../features/settings/ObservabilityPage'

const links = [
  ['/', 'Providers'],
  ['/models', 'Models'],
  ['/calls', 'LLM Calls'],
  ['/settings', 'Settings'],
] as const

export function App() {
  return (
    <div className="min-h-screen bg-zinc-950 text-zinc-100">
      <header className="border-b border-zinc-800 bg-zinc-950/90">
        <div className="mx-auto flex max-w-7xl items-center gap-8 px-6 py-4">
          <div className="text-lg font-semibold">Cursor BYOK</div>
          <nav className="flex gap-2">
            {links.map(([to, label]) => (
              <NavLink
                key={to}
                to={to}
                className={({ isActive }) =>
                  `rounded-md px-3 py-2 text-sm ${isActive ? 'bg-zinc-800 text-white' : 'text-zinc-400 hover:text-white'}`
                }
              >
                {label}
              </NavLink>
            ))}
          </nav>
        </div>
      </header>
      <main className="mx-auto max-w-7xl px-6 py-8">
        <Routes>
          <Route path="/" element={<ProvidersPage />} />
          <Route path="/models" element={<ModelsPage />} />
          <Route path="/calls" element={<CallsPage />} />
          <Route path="/calls/:callId" element={<CallDetailPage />} />
          <Route path="/settings" element={<ObservabilityPage />} />
        </Routes>
      </main>
    </div>
  )
}
