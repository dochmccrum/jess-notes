// Session helpers: the device token lives in IndexedDB (`meta`), never in URLs or cookies.
import { openDb, metaGet, metaPut, metaDelete } from '../worker/idb'

export interface AuthState {
  needs_setup: boolean
  setup_code_required: boolean
  vault_id: string
}

export async function getToken(): Promise<string | null> {
  const db = await openDb()
  return ((await metaGet<string>(db, 'token')) ?? null) as string | null
}

export async function setToken(t: string | null): Promise<void> {
  const db = await openDb()
  if (t) await metaPut(db, 'token', t)
  else await metaDelete(db, 'token')
}

async function json<T>(r: Response): Promise<T> {
  if (!r.ok) {
    let msg = `${r.status}`
    try {
      msg = (await r.json()).error ?? msg
    } catch {
      /* not json */
    }
    throw new Error(msg)
  }
  return r.json() as Promise<T>
}

export const authState = (base = '') => fetch(`${base}/api/auth/state`).then((r) => json<AuthState>(r))

export function deviceName(): string {
  const ua = navigator.userAgent
  const os = /Android/.test(ua) ? 'Android' : /iPad|iPhone|Macintosh/.test(ua) ? 'Apple' : /Linux/.test(ua) ? 'Linux' : /Windows/.test(ua) ? 'Windows' : 'Device'
  const br = /Firefox/.test(ua) ? 'Firefox' : /Edg\//.test(ua) ? 'Edge' : /Chrome/.test(ua) ? 'Chrome' : /Safari/.test(ua) ? 'Safari' : 'Browser'
  return `${br} on ${os}`
}

type TokenResp = { token: string; device_id: string; vault_id: string }

export const setup = (password: string, setupCode: string | null, base = '') =>
  fetch(`${base}/api/auth/setup`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ password, setup_code: setupCode, device_name: deviceName() }) }).then((r) => json<TokenResp>(r))

export const login = (password: string, base = '') =>
  fetch(`${base}/api/auth/login`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ password, device_name: deviceName() }) }).then((r) => json<TokenResp>(r))

export const redeem = (code: string, base = '') =>
  fetch(`${base}/api/auth/redeem`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ code, device_name: deviceName() }) }).then((r) => json<TokenResp>(r))

export const createPairing = (token: string, base = '') =>
  fetch(`${base}/api/auth/pair`, { method: 'POST', headers: { authorization: `Bearer ${token}` } }).then((r) => json<{ code: string; expires_at: number }>(r))

export const listDevices = (token: string, base = '') =>
  fetch(`${base}/api/devices`, { headers: { authorization: `Bearer ${token}` } }).then((r) => json<{ id: string; name: string; last_seen: number | null; revoked_at: number | null }[]>(r))

export const revokeDevice = (token: string, id: string, base = '') => fetch(`${base}/api/devices/${id}/revoke`, { method: 'POST', headers: { authorization: `Bearer ${token}` } })
