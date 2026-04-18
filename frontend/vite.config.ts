import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { execSync } from 'child_process'
import { readFileSync } from 'fs'
import { fileURLToPath, URL } from 'node:url'

// Read version and git info at build time
const getVersionInfo = () => {
  try {
    const version = readFileSync('../VERSION', 'utf-8').trim()
    const gitBranch = execSync('git rev-parse --abbrev-ref HEAD').toString().trim()
    const gitCommit = execSync('git rev-parse --short HEAD').toString().trim()
    return { version, gitBranch, gitCommit }
  } catch {
    return { version: '3.0.0', gitBranch: 'unknown', gitCommit: 'unknown' }
  }
}

const { version, gitBranch, gitCommit } = getVersionInfo()

// https://vite.dev/config/
export default defineConfig({
  plugins: [
    react(),
  ],

  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
    },
    // Ensure only one React copy is used to avoid invalid hook call issues.
    dedupe: ['react', 'react-dom'],
  },

  define: {
    __APP_VERSION__: JSON.stringify(version),
    __GIT_BRANCH__: JSON.stringify(gitBranch),
    __GIT_COMMIT__: JSON.stringify(gitCommit),
  },

  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: 'http://192.168.66.1:3000',
        changeOrigin: true,
      },
    },
  },

  build: {
    // outDir: '../www',
    // emptyOutDir: true,
    rollupOptions: {
      output: {
        // Split heavy dependencies by role to keep the initial chunk smaller.
        manualChunks(id) {
          if (!id.includes('node_modules')) {
            return undefined
          }

          if (id.includes('@mui/x-charts') || id.includes('@mui/x-data-grid')) {
            return 'vendor-mui-x'
          }

          if (id.includes('@mui/icons-material')) {
            return 'vendor-icons'
          }

          if (id.includes('@mui/material') || id.includes('@emotion/')) {
            return 'vendor-mui'
          }

          if (id.includes('@tanstack/react-query')) {
            return 'vendor-query'
          }

          if (
            id.includes('react-router-dom')
            || id.includes('\\react\\')
            || id.includes('/react/')
            || id.includes('\\react-dom\\')
            || id.includes('/react-dom/')
          ) {
            return 'vendor-react'
          }

          return undefined
        },
      },
    },
  },
})
