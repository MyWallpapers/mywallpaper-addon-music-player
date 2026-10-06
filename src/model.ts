import type { JsonValue } from '../generated/mywallpaper-runtime'

export const layouts = ['minimal', 'visualizer', 'centered', 'artwork', 'frosted', 'compact', 'editorial'] as const
export type Layout = typeof layouts[number]
export type RepeatMode = 'none' | 'track' | 'list'
export type MediaAction = 'playPause' | 'previous' | 'next' | 'seek' | 'shuffle' | 'repeat' | 'refresh' | 'session'
export interface MediaState {
  status: 'ready' | 'empty' | 'unavailable'
  sessionId: string
  source: string
  title: string
  artist: string
  album: string
  artwork: string | null
  positionMs: number
  durationMs: number
  playing: boolean
  shuffle: boolean
  repeat: RepeatMode
  capabilities: Record<'playPause' | 'previous' | 'next' | 'seek' | 'shuffle' | 'repeat', boolean>
  sessions: Array<{ id: string; label: string }>
  seekStartMs: number
  seekEndMs: number
}
export const emptyState = (): MediaState => ({ status: 'empty', sessionId: '', source: '', title: '', artist: '', album: '', artwork: null, positionMs: 0, durationMs: 0, playing: false, shuffle: false, repeat: 'none', capabilities: { playPause: false, previous: false, next: false, seek: false, shuffle: false, repeat: false }, sessions: [], seekStartMs: 0, seekEndMs: 0 })
const object = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v)
const text = (v: unknown, max = 1024): string => typeof v === 'string' ? v.slice(0, max) : ''
const milliseconds = (v: unknown): number => typeof v === 'number' && Number.isFinite(v) ? Math.max(0, Math.min(v, 864000000)) : 0
export function decodeState(payload: JsonValue, previous?: MediaState): MediaState | null {
  if (!object(payload) || payload.kind !== 'media.state') return null
  if (payload.status !== 'ready') return { ...emptyState(), status: payload.status === 'unavailable' ? 'unavailable' : 'empty' }
  const c = object(payload.capabilities) ? payload.capabilities : {}
  const durationMs = milliseconds(payload.durationMs)
  const state = emptyState()
  for (const key of Object.keys(state.capabilities) as Array<keyof MediaState['capabilities']>) state.capabilities[key] = c[key] === true
  const sameTrack = previous?.sessionId === text(payload.sessionId) && previous.title === text(payload.title) && previous.artist === text(payload.artist) && previous.album === text(payload.album)
  const artwork = payload.artwork === undefined && sameTrack ? previous!.artwork : typeof payload.artwork === 'string' && payload.artwork.length < 750000 && /^data:image\/(png|jpeg|jpg|webp|bmp);base64,[a-z0-9+/=]+$/i.test(payload.artwork) ? payload.artwork : null
  return { ...state, status: payload.status === 'ready' ? 'ready' : payload.status === 'unavailable' ? 'unavailable' : 'empty', sessionId: text(payload.sessionId), source: text(payload.source), title: text(payload.title), artist: text(payload.artist), album: text(payload.album), artwork, durationMs, positionMs: milliseconds(payload.positionMs), playing: payload.playing === true, shuffle: payload.shuffle === true, repeat: payload.repeat === 'track' || payload.repeat === 'list' ? payload.repeat : 'none', sessions: Array.isArray(payload.sessions) ? payload.sessions.slice(0, 32).flatMap(s => object(s) ? [{ id: text(s.id), label: text(s.label) }] : []) : [], seekStartMs: milliseconds(payload.seekStartMs), seekEndMs: milliseconds(payload.seekEndMs) || durationMs }
}
export function formatTime(ms: number): string {
  const seconds = Math.floor(Math.max(0, ms) / 1000)
  const minutes = Math.floor(seconds / 60)
  return `${minutes}:${String(seconds % 60).padStart(2, '0')}`
}
export function favoriteKey(state: MediaState): string { return JSON.stringify([state.title, state.artist, state.album]) }
export function readFavorites(value: unknown): string[] {
  if (typeof value !== 'string' || value.length > 100000) return []
  try { const values: unknown = JSON.parse(value); return Array.isArray(values) ? values.filter((v): v is string => typeof v === 'string' && v.length <= 4096).slice(0, 100) : [] } catch { return [] }
}
