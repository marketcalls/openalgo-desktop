import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'path'

// Desktop: in development the Rust server runs on the maintainer's dev port
// (5500, next to OpenAlgo web on 5000), so the dev server proxies there.
const BACKEND = process.env.OPENALGO_DEV_BACKEND || 'http://127.0.0.1:5500'

// Desktop: prefixes that are both a React page and a backend API (for example
// /logs, /health, /playground). Data requests go to the Rust server; a browser
// page navigation (Accept: text/html) stays on Vite so React Router renders it.
const SHARED_PREFIXES = [
  '/admin',
  '/logs',
  '/traffic',
  '/latency',
  '/security',
  '/health',
  '/playground',
  '/leverage',
  '/search',
  '/sandbox',
  '/analyzer',
  '/historify',
  '/action-center',
  '/apikey',
  '/setup',
  '/pnltracker',
  '/watchlist',
  '/alerts',
  '/chart',
  '/openscript',
  '/scalping',
  '/strategy',
  '/strategybuilder',
  '/chartink',
  '/oiprofile',
  '/oitracker',
  '/ivchart',
  '/gammadensity',
  '/straddle',
  '/straddlepnl',
  '/volsurface',
  '/gex',
  '/ivsmile',
  '/arbitrage',
  '/telegram',
  '/whatsapp',
  '/agent',
  '/close_position',
  '/close_all_positions',
  '/cancel_all_orders',
  '/cancel_order',
  '/modify_order',
  '/modify_gtt_order',
  '/cancel_gtt_order',
]

const pageOrApi = {
  target: BACKEND,
  changeOrigin: true,
  bypass(req: { headers: { accept?: string }; url?: string }) {
    if (req.headers.accept?.includes('text/html')) return req.url
    return undefined
  },
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    react(),
    tailwindcss(),
    // No build-time compression plugin. The .br/.gz variants it used to emit
    // were force-committed with frontend/dist/ by CI, and because compressed
    // output can be neither deflated nor delta-compressed by git, they grew
    // into two thirds of the repository history and tripled clone times.
    // utils/precompress_assets.py regenerates the gzip variants at app
    // startup instead, from the tracked raw assets, in about 30ms once warm.
  ],
  // plotly.js-dist-min's UMD wrapper has an unguarded `global.matchMedia`
  // reference. Vite 8 no longer shims Node's `global` in the browser, so the
  // /tools pages that load Plotly (StrategyBuilder, MaxPain, OI Tracker, etc.)
  // threw "global is not defined". Map `global` to the browser `globalThis`.
  define: {
    global: 'globalThis',
  },
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  // Desktop: keep Rust compiler output visible under `tauri dev`.
  clearScreen: false,
  server: {
    port: 5173,
    // Desktop: Tauri's devUrl is fixed to 5173, so fail rather than move ports.
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
    proxy: {
      '/api': {
        target: BACKEND,
        changeOrigin: true,
      },
      '/socket.io': {
        target: BACKEND,
        ws: true,
      },
      '/auth': {
        target: BACKEND,
        changeOrigin: true,
      },
      // User indicator modules are served by Flask from strategies/indicators,
      // never bundled, so the dev server has to pass them through too.
      '/custom-indicators': {
        target: BACKEND,
        changeOrigin: true,
      },
      // Desktop: the in-app Server Settings page's endpoint.
      '/settings/api': {
        target: BACKEND,
        changeOrigin: true,
      },
      // Desktop: every other backend prefix, with page navigations bypassed.
      ...Object.fromEntries(SHARED_PREFIXES.map((p) => [p, pageOrApi])),
    },
  },
  build: {
    outDir: 'dist',
    sourcemap: false,
    // Plotly core can legitimately produce a large shared chart chunk.
    // Keep the limit high enough for that known vendor cost while still
    // flagging any new app-code chunk that drifts above 1MB.
    chunkSizeWarningLimit: 1100,
    rollupOptions: {
      // Desktop: the OpenScript runner page the app opens in a hidden window
      // per live run, built beside the app as its own small entry.
      input: {
        main: path.resolve(__dirname, 'index.html'),
        runner: path.resolve(__dirname, 'openscript-runner.html'),
      },
      output: {
        // Split the stable framework libs into their own long-cached chunk
        // so an app-code change doesn't bust react/router/query for returning
        // users, and the browser can fetch vendor + page chunks in parallel.
        // Vite already splits the heavy charting libs (plotly, lightweight-
        // charts) automatically, so we only carve out the framework core here.
        manualChunks(id) {
          if (!id.includes('node_modules')) return
          if (/[\\/]node_modules[\\/](react|react-dom|react-router|react-router-dom|scheduler)[\\/]/.test(id)) {
            return 'react-vendor'
          }
          if (id.includes('tanstack/react-query')) return 'tanstack'
        },
      },
    },
  },
})
