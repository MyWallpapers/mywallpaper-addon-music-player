import type { Plugin } from 'vite'

export interface MyWallpaperBuildOptions {
  /** Omit for the conventional visual entry; false for a headless service. */
  canvas?: string | false
  /** A module exporting start(context), running in the common Canvas document. */
  service?: string
}

/** Finite ESM builds. `mywallpaper dev` serves immutable build revisions. */
export function myWallpaperAddon(options: MyWallpaperBuildOptions = {}): Plugin {
  const canvas = options.canvas ?? 'src/main.ts'
  const inputs = { ...(canvas ? { addon: canvas } : {}), ...(options.service ? { service: options.service } : {}) }
  if (Object.keys(inputs).length === 0) throw new Error('Declare a Canvas or service entry to build.')
  return {
    name: 'mywallpaper-addon',
    apply: 'build',
    config() {
      return {
        base: './',
        build: {
          target: 'es2022',
          rollupOptions: {
            preserveEntrySignatures: 'strict',
            input: inputs,
            output: {
              entryFileNames: 'assets/[name].js',
              chunkFileNames: 'assets/chunk-[name]-[hash].js',
              assetFileNames: 'assets/[name]-[hash][extname]',
            },
          },
        },
      }
    },
    generateBundle(_options, bundle) {
      for (const [name, exported] of Object.entries({ ...(canvas ? { addon: 'mount' } : {}), ...(options.service ? { service: 'start' } : {}) })) {
        const entry = bundle[`assets/${name}.js`]
        if (!entry || entry.type !== 'chunk' || !entry.exports.includes(exported)) {
          this.error(`The production ${name} entry must export ${exported}.`)
        }
      }
      const entry = bundle['assets/addon.js']
      const styles = Object.values(bundle)
        .filter((output) => output.type === 'asset' && output.fileName.endsWith('.css'))
        .map((output) => output.fileName.split('/').at(-1))
        .filter((fileName): fileName is string => fileName !== undefined)
      if ((!entry || entry.type !== 'chunk') && styles.length > 0) {
        this.error('A headless service cannot load DOM stylesheets.')
      }
      if (!entry || entry.type !== 'chunk') return
      const stylesheetBootstrap = styles.map((fileName) => `{
  const href = new URL(${JSON.stringify(`./${fileName}`)}, import.meta.url).href
  let link = [...document.querySelectorAll('link[rel~="stylesheet"]')]
    .find((candidate) => candidate.href === href)
  if (!link) {
    link = document.createElement('link')
    link.rel = 'stylesheet'
    link.href = href
    document.head.append(link)
  }
  if (!link.sheet) await new Promise((resolve, reject) => {
    link.addEventListener('load', resolve, { once: true })
    link.addEventListener('error', () => reject(new Error('Failed to load add-on stylesheet: ' + href)), { once: true })
  })
}
`).join('')
      entry.code = `${stylesheetBootstrap}\n${entry.code}`
    },
  }
}
