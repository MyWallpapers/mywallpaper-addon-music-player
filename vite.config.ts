import { defineConfig } from 'vite'
import { myWallpaperAddon } from './scripts/addon-build'

export default defineConfig({ plugins: [myWallpaperAddon()] })
