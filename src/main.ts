import type { CanvasAddonMountContext, JsonValue, NativeConnection } from '../generated/mywallpaper-runtime'
import { decodeState, emptyState, favoriteKey, formatTime, layouts, readFavorites, type Layout, type MediaAction, type MediaState } from './model'
import { icon } from './icons'
import demoCover from '../assets/demo-cover.webp'
import './styles.css'

const copy = {
  en: { waiting: 'Connecting to Windows…', empty: 'Your music, right here.', emptyHint: 'Play a track in Spotify or another Windows media player.', offline: 'Open in MyWallpaper Desktop', offlineHint: 'Windows playback controls require the native companion.', reconnecting: 'Reconnecting…', play: 'Play', pause: 'Pause', previous: 'Previous track', next: 'Next track', shuffle: 'Shuffle', repeat: 'Repeat', repeatOff: 'Off', repeatTrack: 'Track', repeatList: 'Playlist', spectrumError: 'Audio visualization is unavailable. Playback controls remain available.', unavailable: 'Windows media controls are unavailable', seek: 'Playback position', favorite: 'Save track on this device', unfavorite: 'Remove saved track', more: 'Player options', layouts: 'Appearance', source: 'Media source', unknown: 'Unknown artist', failed: 'This player could not perform that action.', retry: 'Refresh media session', visualizer: 'System audio spectrum', noSpectrum: 'Waiting for system audio', labels: ['Minimal', 'Artwork + spectrum', 'Full artwork', 'Compact bar'] },
  fr: { waiting: 'Connexion à Windows…', empty: 'Votre musique, juste ici.', emptyHint: 'Lancez un morceau dans Spotify ou un autre lecteur Windows.', offline: 'Ouvrir dans MyWallpaper Desktop', offlineHint: 'Le contrôle de la musique nécessite le compagnon natif Windows.', reconnecting: 'Reconnexion…', play: 'Lire', pause: 'Mettre en pause', previous: 'Morceau précédent', next: 'Morceau suivant', shuffle: 'Lecture aléatoire', repeat: 'Répétition', repeatOff: 'Désactivée', repeatTrack: 'Morceau', repeatList: 'Playlist', spectrumError: 'Le spectre audio est indisponible. Les commandes de lecture restent disponibles.', unavailable: 'Le contrôle multimédia Windows est indisponible', seek: 'Position de lecture', favorite: 'Enregistrer ce morceau sur cet appareil', unfavorite: 'Retirer le morceau enregistré', more: 'Options du lecteur', layouts: 'Présentation', source: 'Source musicale', unknown: 'Artiste inconnu', failed: 'Ce lecteur n’a pas pu effectuer cette action.', retry: 'Actualiser la session musicale', visualizer: 'Spectre audio du système', noSpectrum: 'En attente du son du système', labels: ['Minimaliste', 'Pochette + spectre', 'Grand visuel', 'Barre compacte'] },
}
const colorSetting = (value: unknown, fallback: string) => typeof value === 'string' && /^#[0-9a-f]{6}$/i.test(value) ? value : fallback
const numberSetting = (value: unknown, fallback: number, max = 1) => typeof value === 'number' && Number.isFinite(value) ? Math.min(max, Math.max(0, value)) : fallback
const colorChannels = (value: string) => [1, 3, 5].map(start => Number.parseInt(value.slice(start, start + 2), 16))
const button = (name: string, className = '') => `<button type="button" class="mp-button ${className}" data-action="${name}">${icon(name === 'playPause' ? 'play' : name)}</button>`

export function mount({ layer, runtime }: CanvasAddonMountContext): () => void {
  const abort = new AbortController()
  const { signal } = abort
  let disposed = false
  let media = emptyState()
  let settings = layer.settings.get()
  let deviceSettings = layer.deviceSettings.get()
  let language: 'en' | 'fr' = 'en'
  let layout: Layout = 'minimal'
  let background = 'transparent'
  let connection: NativeConnection | null = null
  let disconnectMessages: (() => void) | undefined
  let disconnectState: (() => void) | undefined
  let receivedAt = performance.now()
  let scrubbing = false
  let commandSequence = 0
  let activeRequest: { id: string; timer: ReturnType<typeof setTimeout> } | null = null
  let spectrumAt = 0
  let frame = 0
  let pendingBands: number[] | null = null
  let connectionStatus = 'connecting'
  const wrapper = document.createElement('div')
  wrapper.className = 'mp-stage'
  const card = document.createElement('article')
  card.className = 'mp-card'
  card.innerHTML = `<div class="mp-backdrop" aria-hidden="true"><img class="mp-artwork" alt="" hidden><div class="mp-shade"></div></div><div class="mp-content"><div class="mp-top"><img class="mp-cover" alt="" hidden><div class="mp-info"><h2 class="mp-title"></h2><p class="mp-artist"></p><p class="mp-album"></p></div><div class="mp-actions">${button('heart')}${button('more')}</div></div><div class="mp-spectrum" role="img" hidden>${Array.from({length:32},()=>'<i></i>').join('')}</div><div class="mp-bottom"><div class="mp-timeline"><input type="range" min="0" max="100" step="1000" value="0" class="mp-seek"><div class="mp-times"><span class="mp-elapsed">0:00</span><span class="mp-duration">0:00</span></div></div><div class="mp-controls">${button('shuffle')}${button('previous')}${button('playPause', 'mp-play')}${button('next')}${button('repeat')}</div></div><p class="mp-status" role="status"></p><div class="mp-menu" role="menu" hidden></div></div>`
  wrapper.append(card)
  layer.root.replaceChildren(wrapper)
  const get = <T extends Element>(selector: string) => card.querySelector<T>(selector)!
  const title = get<HTMLElement>('.mp-title')
  const artist = get<HTMLElement>('.mp-artist')
  const album = get<HTMLElement>('.mp-album')
  const artwork = get<HTMLImageElement>('.mp-artwork')
  const cover = get<HTMLImageElement>('.mp-cover')
  const seek = get<HTMLInputElement>('.mp-seek')
  const elapsed = get<HTMLElement>('.mp-elapsed')
  const duration = get<HTMLElement>('.mp-duration')
  const status = get<HTMLElement>('.mp-status')
  const menu = get<HTMLElement>('.mp-menu')
  const spectrum = get<HTMLElement>('.mp-spectrum')
  const bands = Array.from(spectrum.children) as HTMLElement[]
  const controls = new Map<MediaAction | 'heart' | 'more', HTMLButtonElement>(Array.from(card.querySelectorAll<HTMLButtonElement>('[data-action]')).map(b => [b.dataset.action as MediaAction | 'heart' | 'more', b]))
  const label = (action: MediaAction | 'heart' | 'more', text: string) => {
    const b = controls.get(action)!
    b.title = text
    b.setAttribute('aria-label', text)
  }
  controls.get('more')!.setAttribute('aria-haspopup', 'menu')
  controls.get('more')!.setAttribute('aria-expanded', 'false')
  const translation = () => copy[language]
  const closeMenu = (focus = false) => {
    menu.hidden = true
    controls.get('more')!.setAttribute('aria-expanded', 'false')
    if (focus) controls.get('more')!.focus()
  }
  function showMessage(message: string) { status.textContent = message; status.hidden = !message }
  function displayedPosition() { return Math.min(media.durationMs || media.positionMs, media.positionMs + (media.playing && connectionStatus === 'open' ? performance.now() - receivedAt : 0)) }
  function updateProgress() {
    if (disposed || scrubbing || document.hidden) return
    const position = displayedPosition()
    seek.value = String(position)
    elapsed.textContent = formatTime(position)
    seek.style.setProperty('--mp-progress', `${media.durationMs ? position / media.durationMs * 100 : 0}%`)
    seek.setAttribute('aria-valuetext', `${formatTime(position)} / ${formatTime(media.durationMs)}`)
    if (layout === 'visualizer' && performance.now() - spectrumAt > 2000) { for (const b of bands) b.style.setProperty('--mp-band', '0'); spectrum.title = translation().noSpectrum }
  }
  function render() {
    const t = translation()
    const ready = media.status === 'ready'
    card.dataset.layout = layout
    card.dataset.ready = String(ready)
    title.textContent = ready ? media.title || media.source : connectionStatus === 'connecting' ? t.waiting : connectionStatus === 'failed' ? t.offline : media.status === 'unavailable' ? t.unavailable : t.empty
    title.title = title.textContent ?? ''
    artist.textContent = ready ? media.artist || media.source || t.unknown : connectionStatus === 'failed' ? t.offlineHint : t.emptyHint
    album.textContent = media.album
    album.hidden = settings.showAlbum === false || !media.album
    for (const image of [artwork, cover]) {
      if (media.artwork && image.getAttribute('src') !== media.artwork) image.src = media.artwork
      image.hidden = !media.artwork || (image === artwork && background !== 'artwork')
      if (!media.artwork) image.removeAttribute('src')
    }
    card.dataset.art = String(Boolean(media.artwork))
    const playButton = controls.get('playPause')!
    if (playButton.dataset.playing !== String(media.playing)) { playButton.innerHTML = icon(media.playing ? 'pause' : 'play'); playButton.dataset.playing = String(media.playing) }
    label('playPause', media.playing ? t.pause : t.play)
    label('previous', t.previous); label('next', t.next); label('shuffle', t.shuffle); label('repeat', `${t.repeat}: ${media.repeat === 'none' ? t.repeatOff : media.repeat === 'track' ? t.repeatTrack : t.repeatList}`); label('more', t.more)
    for (const action of ['playPause', 'previous', 'next', 'shuffle', 'repeat'] as const) controls.get(action)!.disabled = !ready || !media.capabilities[action] || connectionStatus !== 'open' || activeRequest !== null
    controls.get('shuffle')!.setAttribute('aria-pressed', String(media.shuffle))
    controls.get('repeat')!.setAttribute('aria-pressed', String(media.repeat !== 'none'))
    controls.get('repeat')!.dataset.repeat = media.repeat
    const isFavorite = readFavorites(deviceSettings.favorites).includes(favoriteKey(media))
    controls.get('heart')!.setAttribute('aria-pressed', String(isFavorite))
    controls.get('heart')!.disabled = !ready
    controls.get('heart')!.hidden = settings.showFavorite === false
    label('heart', isFavorite ? t.unfavorite : t.favorite)
    seek.max = String(media.seekEndMs || media.durationMs || 100)
    seek.min = String(media.seekStartMs)
    seek.disabled = !ready || !media.capabilities.seek || media.durationMs <= 0 || connectionStatus !== 'open' || activeRequest !== null
    seek.setAttribute('aria-label', t.seek)
    duration.textContent = formatTime(media.durationMs)
    spectrum.hidden = layout !== 'visualizer'
    spectrum.setAttribute('aria-label', t.visualizer)
    updateProgress()
  }
  function applySettings() {
    language = settings.language === 'fr' || (settings.language !== 'en' && navigator.language.startsWith('fr')) ? 'fr' : 'en'
    card.lang = language
    layout = layouts.includes(settings.layout as Layout) ? settings.layout as Layout : 'minimal'
    background = settings.background === 'color' || settings.background === 'artwork' ? settings.background : 'transparent'
    card.dataset.background = background
    const surface = colorSetting(settings.backgroundColor, '#121214')
    const foreground = colorSetting(settings.foreground, '#ffffff')
    const accent = colorSetting(settings.accent, '#ffffff')
    const opacity = numberSetting(settings.opacity, .65)
    const luminance = colorChannels(accent)
      .map(value => value / 255)
      .map(value => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4)
      .reduce((sum, value, index) => sum + value * [.2126, .7152, .0722][index], 0)
    card.style.setProperty('--mp-background', colorChannels(surface).join(' '))
    card.style.setProperty('--mp-accent', accent)
    card.style.setProperty('--mp-play-ink', luminance > .179 ? '#000000' : '#ffffff')
    card.style.setProperty('--mp-foreground', foreground)
    card.style.setProperty('--mp-foreground-rgb', colorChannels(foreground).join(' '))
    card.style.setProperty('--mp-opacity', String(opacity))
    card.style.setProperty('--mp-border', String(numberSetting(settings.borderOpacity, 0)))
    card.style.setProperty('--mp-blur', `${opacity > 0 ? numberSetting(settings.blur, 16, 24) : 0}px`)
    render()
  }
  function settleRequest(error?: string) {
    if (activeRequest) clearTimeout(activeRequest.timer)
    activeRequest = null
    if (error) showMessage(error)
    render()
  }
  async function send(action: MediaAction, value?: string | number | boolean) {
    if (!connection || connection.state !== 'open' || activeRequest) return
    const id = `${layer.layerId}:${++commandSequence}`
    activeRequest = { id, timer: setTimeout(() => { if (activeRequest?.id === id) settleRequest(translation().failed) }, 5000) }
    showMessage(''); render()
    try { await connection.send({ kind: 'media.command', requestId: id, action, ...(value !== undefined ? { value } : {}) }); } catch { if (activeRequest?.id === id) settleRequest(translation().failed) }
  }
  function receive(payload: JsonValue) {
    if (disposed) return
    const state = decodeState(payload, media)
    if (state) { media = state; receivedAt = performance.now(); render(); return }
    if (typeof payload !== 'object' || payload === null || Array.isArray(payload)) return
    if (payload.kind === 'media.result' && payload.requestId === activeRequest?.id) settleRequest(payload.ok === true ? undefined : translation().failed)
    if (payload.kind === 'media.spectrum.error' && layout === 'visualizer') showMessage(translation().spectrumError)
    if (payload.kind === 'media.error') showMessage(translation().failed)
    if (payload.kind === 'media.spectrum' && layout === 'visualizer' && Array.isArray(payload.bands) && !document.hidden) {
      pendingBands = payload.bands.slice(0, 32).map(v => typeof v === 'number' && Number.isFinite(v) ? Math.max(0, Math.min(1, v)) : 0)
      if (status.textContent === translation().spectrumError) showMessage('')
      spectrumAt = performance.now()
      if (!frame) frame = requestAnimationFrame(() => { frame = 0; const values = pendingBands; pendingBands = null; if (disposed || !values) return; bands.forEach((b, i) => b.style.setProperty('--mp-band', String(values[i] ?? 0))); spectrum.title = translation().visualizer })
    }
  }
  async function connect() {
    try {
      const next = await layer.native.companion.connect()
      if (disposed) { next.close(); return }
      connection = next
      connectionStatus = next.state
      disconnectMessages = next.onMessage(receive)
      disconnectState = next.onStateChange(state => { connectionStatus = state; if (state !== 'open') settleRequest(); showMessage(state === 'reconnecting' ? translation().reconnecting : state === 'failed' || state === 'closed' ? translation().offlineHint : ''); render() })
      render()
      await send('refresh')
    } catch { if (!disposed) { connectionStatus = 'failed'; render() } }
  }
  function openMenu() {
    if (!menu.hidden) { closeMenu(); return }
    menu.replaceChildren()
    const t = translation()
    const section = document.createElement('p'); section.className = 'mp-menu-label'; section.textContent = t.layouts; menu.append(section)
    layouts.forEach((value, i) => {
      const b = document.createElement('button'); b.type = 'button'; b.className = 'mp-menu-item'; b.setAttribute('role', 'menuitemradio'); b.setAttribute('aria-checked', String(layout === value)); b.textContent = t.labels[i]
      b.dataset.layout = value; menu.append(b)
    })
    if (media.sessions.length > 1) {
      const section = document.createElement('p'); section.className = 'mp-menu-label'; section.textContent = t.source; menu.append(section)
      for (const s of media.sessions) { const b = document.createElement('button'); b.type = 'button'; b.className = 'mp-menu-item'; b.setAttribute('role','menuitemradio'); b.setAttribute('aria-checked',String(s.id===media.sessionId)); b.textContent=s.label; b.dataset.session=s.id; menu.append(b) }
    }
    const refresh = document.createElement('button'); refresh.type='button';refresh.className='mp-menu-item';refresh.setAttribute('role','menuitem');refresh.textContent=t.retry;refresh.dataset.refresh='true';menu.append(refresh)
    menu.hidden = false; controls.get('more')!.setAttribute('aria-expanded','true'); menu.querySelector<HTMLButtonElement>('button[aria-checked="true"]')?.focus()
  }
  card.addEventListener('click', (event) => {
    const b = (event.target as Element).closest<HTMLButtonElement>('[data-action]')
    if (!b || b.disabled) return
    const action = b.dataset.action
    if (action === 'more') { openMenu(); return }
    if (action === 'heart') {
      const values = readFavorites(deviceSettings.favorites); const key = favoriteKey(media)
      const updated = values.includes(key) ? values.filter(v => v !== key) : [...values.slice(-99), key]
      while (JSON.stringify(updated).length > 48000) updated.shift()
      void layer.deviceSettings.set({favorites:JSON.stringify(updated)}).catch(()=>showMessage(translation().failed)); return
    }
    if (action === 'shuffle') void send('shuffle', !media.shuffle)
    else if (action === 'repeat') void send('repeat', media.repeat === 'none' ? 'list' : media.repeat === 'list' ? 'track' : 'none')
    else if (action === 'playPause' || action === 'previous' || action === 'next') void send(action)
  }, {signal})
  seek.addEventListener('input',()=>{scrubbing=true;elapsed.textContent=formatTime(Number(seek.value));seek.style.setProperty('--mp-progress',`${media.durationMs?Number(seek.value)/media.durationMs*100:0}%`)},{signal})
  seek.addEventListener('change',()=>{scrubbing=false;void send('seek',Number(seek.value))},{signal})
  seek.addEventListener('blur',()=>{scrubbing=false;updateProgress()},{signal})
  document.addEventListener('pointerdown',event=>{if(!card.contains(event.target as Node))closeMenu()},{signal})
  menu.addEventListener('click', event => {
    const item = (event.target as Element).closest<HTMLButtonElement>('button')
    if (!item) return
    closeMenu(true)
    if (item.dataset.layout) void layer.settings.set({layout:item.dataset.layout}).catch(() => showMessage(translation().failed))
    else if (item.dataset.session) void send('session',item.dataset.session)
    else if (item.dataset.refresh) void send('refresh')
  }, {signal})
  menu.addEventListener('keydown', event=>{
    if(event.key==='Escape'){event.preventDefault();closeMenu(true)}
    if(event.key==='ArrowDown'||event.key==='ArrowUp'||event.key==='Home'||event.key==='End'){
      event.preventDefault(); const buttons=Array.from(menu.querySelectorAll<HTMLButtonElement>('button')); const i=buttons.indexOf(document.activeElement as HTMLButtonElement); const next=event.key==='Home'?0:event.key==='End'?buttons.length-1:(i+(event.key==='ArrowDown'?1:-1)+buttons.length)%buttons.length;buttons[next]?.focus()
    }
    if(event.key==='Tab')closeMenu()
  },{signal})
  for(const image of [cover,artwork])image.addEventListener('error',()=>{image.hidden=true},{signal})
  const stopSettings=layer.settings.subscribe(values=>{settings=values;applySettings()})
  const stopDeviceSettings=layer.deviceSettings.subscribe(values=>{deviceSettings=values;render()})
  const timer=setInterval(updateProgress,250)
  applySettings()
  if(runtime.mode==='thumbnail'){
    connectionStatus='open';media={...emptyState(),status:'ready',title:'Better Days',artist:'Luna River',album:'Horizons',artwork:demoCover,positionMs:84000,durationMs:236000};render()
  }else void connect()
  const cleanup=()=>{if(disposed)return;disposed=true;abort.abort();clearInterval(timer);if(frame)cancelAnimationFrame(frame);if(activeRequest)clearTimeout(activeRequest.timer);stopSettings();stopDeviceSettings();disconnectMessages?.();disconnectState?.();connection?.close();wrapper.remove()}
  const stopDispose=layer.lifecycle.onDispose(cleanup)
  return ()=>{stopDispose();cleanup()}
}
