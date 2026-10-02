// Spaces (DESIGN §24): the vaults this app holds, one open at a time. Apps only: the web app is
// served by its server and has exactly one (remote) vault. Adding, switching and moving restart
// the app into the space, so every store belongs to one space.
import { deviceName } from './auth'
import { androidRestart, isTauri } from './platform'

export type SpaceKind = 'local' | 'remote'
export interface Space {
  id: string
  name: string
  kind: SpaceKind
  /** Remote: the server's address. */
  server?: string | null
}
export interface SpacesState {
  active: string | null
  spaces: Space[]
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core')
  return invoke<T>(cmd, args)
}

export const spacesSupported = isTauri

/** The open space, or null (a fresh install, or the web). */
export async function currentSpace(): Promise<Space | null> {
  return isTauri ? call<Space | null>('space_current') : null
}

export const listSpaces = () => call<SpacesState>('spaces_list')

/** Creates a space on this device and opens it (the app restarts). */
export const addLocalSpace = (name: string) => call<void>('space_add_local', { name }).then(androidRestart)

/**
 * Signs in to a server, then adds it as a space and opens it (the app restarts). `server` is an
 * address or a pairing link (`https://host/#pair=CODE`, which needs no password).
 */
export const addRemoteSpace = (server: string, password: string | null, name?: string) =>
  call<void>('space_add_remote', { server, password, name: name || null, deviceName: deviceName() }).then(androidRestart)

export const switchSpace = (id: string) => call<void>('space_switch', { id }).then(androidRestart)
export const renameSpace = (id: string, name: string) => call<SpacesState>('space_rename', { id, name })
/** Deletes a space and its data on this device. The open space can't be deleted. */
export const deleteSpace = (id: string) => call<SpacesState>('space_delete', { id })

/**
 * Moves the open local space to a server: exported, uploaded and imported there, then opened as
 * a remote space (the app restarts shortly after). The local copy stays, renamed "… (moved)".
 */
export const moveSpaceToServer = async (server: string, password: string | null) => {
  const report = await call<Record<string, unknown>>('space_move_to_server', { server, password, deviceName: deviceName() })
  // Desktop restarts by itself shortly after; Android restarts from here, once the UI said so.
  setTimeout(androidRestart, 1500)
  return report
}

/** True when `s` is a pairing link (it carries its own sign-in). */
export const isPairingLink = (s: string) => /#pair=[^&\s]+/.test(s)

/** Scans a pairing QR code with the camera (Android). Null if cancelled or unavailable. */
export async function scanPairingCode(): Promise<string | null> {
  const { scan, Format, checkPermissions, requestPermissions } = await import('@tauri-apps/plugin-barcode-scanner')
  let p = await checkPermissions()
  if (p !== 'granted') p = await requestPermissions()
  if (p !== 'granted') throw new Error('Camera permission is needed to scan a code')
  try {
    const r = await scan({ formats: [Format.QRCode], windowed: false })
    return r.content || null
  } catch (e) {
    if (/cancel/i.test(String(e))) return null
    throw e
  }
}
