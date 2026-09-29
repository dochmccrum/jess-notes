// Session helpers. On the web the device token lives in IndexedDB (`meta`), never in URLs or
// cookies, and requests go to the same origin. On Tauri the token and server URL live in the
// native app.db and requests go through Rust (no CORS; the WebView never sees other origins).
import { openDb, metaGet, metaPut, metaDelete } from '../worker/idb'
import { isTauri, tauriInvoke } from './platform'

export interface AuthState {
  needs_setup: boolean
  setup_code_required: boolean
  vault_id: string
}

export async function getToken(): Promise<string | null> {
  if (isTauri) return tauriInvoke<string | null>('get_token')
  const db = await openDb()
  return ((await metaGet<string>(db, 'token')) ?? null) as string | null
}

export async function setToken(t: string | null): Promise<void> {
  if (isTauri) return tauriInvoke('set_token', { token: t })
  const db = await openDb()
  if (t) await metaPut(db, 'token', t)
  else await metaDelete(db, 'token')
}

/** The server this app talks to (Tauri only; the web app is served by it). */
export async function getServer(): Promise<string | null> {
  return isTauri ? tauriInvoke<string | null>('get_server') : location.origin
}

export async function setServer(url: string): Promise<void> {
  if (isTauri) await tauriInvoke('set_server', { url })
}

class HttpError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message)
  }
}

async function request<T>(method: 'GET' | 'POST', path: string, body?: unknown, token?: string | null): Promise<T> {
  if (isTauri) {
    const [status, v] = await tauriInvoke<[number, unknown]>('http_json', { method, path, body: body ?? null, auth: !!token })
    if (status < 200 || status >= 300) throw new HttpError(status, status === 429 ? '429' : ((v as { error?: string } | null)?.error ?? String(status)))
    return v as T
  }
  const headers: Record<string, string> = {}
  if (body !== undefined) headers['content-type'] = 'application/json'
  if (token) headers.authorization = `Bearer ${token}`
  const r = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) })
  if (!r.ok) {
    let msg = `${r.status}`
    try {
      msg = (await r.json()).error ?? msg
    } catch {
      /* not json */
    }
    throw new HttpError(r.status, msg)
  }
  const text = await r.text()
  return (text ? JSON.parse(text) : null) as T
}

export const authState = () => request<AuthState>('GET', '/api/auth/state')

export function deviceName(): string {
  const ua = navigator.userAgent
  const os = /Android/.test(ua) ? 'Android' : /iPad|iPhone|Macintosh/.test(ua) ? 'Apple' : /Linux/.test(ua) ? 'Linux' : /Windows/.test(ua) ? 'Windows' : 'Device'
  if (isTauri) return `Jess app on ${os}`
  const br = /Firefox/.test(ua) ? 'Firefox' : /Edg\//.test(ua) ? 'Edge' : /Chrome/.test(ua) ? 'Chrome' : /Safari/.test(ua) ? 'Safari' : 'Browser'
  return `${br} on ${os}`
}

type TokenResp = { token: string; device_id: string; vault_id: string }

export const setup = (password: string, setupCode: string | null) => request<TokenResp>('POST', '/api/auth/setup', { password, setup_code: setupCode, device_name: deviceName() })

export const login = (password: string) => request<TokenResp>('POST', '/api/auth/login', { password, device_name: deviceName() })

export const redeem = (code: string) => request<TokenResp>('POST', '/api/auth/redeem', { code, device_name: deviceName() })

export const createPairing = (token: string) => request<{ code: string; expires_at: number }>('POST', '/api/auth/pair', undefined, token)

export const listDevices = (token: string) => request<{ id: string; name: string; last_seen: number | null; revoked_at: number | null }[]>('GET', '/api/devices', undefined, token)

export const revokeDevice = (token: string, id: string) => request<unknown>('POST', `/api/devices/${id}/revoke`, undefined, token)
