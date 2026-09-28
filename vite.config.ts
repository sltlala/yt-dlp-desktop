import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  // Tauri 期望前端产物用相对路径引用。
  base: './',
  clearScreen: false,
  server: {
    port: 5183,
    strictPort: true,
    host: '127.0.0.1',
    watch: {
      // src-tauri / crates 由 cargo 自己监听，避免重复触发。
      ignored: ['**/src-tauri/**', '**/crates/**', '**/target/**'],
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'chrome110',
    sourcemap: false,
  },
})
