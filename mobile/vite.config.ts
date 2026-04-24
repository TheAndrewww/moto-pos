import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { resolve } from 'path';

// La app se construye dentro de src-tauri/resources/mobile-dist para que
// el servidor Axum la sirva como bundle estático.
export default defineConfig({
  plugins: [react()],
  base: './',
  build: {
    outDir: resolve(__dirname, '../src-tauri/resources/mobile-dist'),
    emptyOutDir: true,
  },
  server: {
    // Para desarrollo standalone apuntando a un POS corriendo en LAN.
    port: 5174,
  },
});
