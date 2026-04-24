import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5180,
    proxy: {
      // En dev, el panel hace fetch a /api/* y /auth/* y /sync/* — los
      // proxyeamos al servidor remoto local para evitar CORS.
      '/api':  { target: 'http://localhost:3000', changeOrigin: true },
      '/auth': { target: 'http://localhost:3000', changeOrigin: true },
    },
  },
})
