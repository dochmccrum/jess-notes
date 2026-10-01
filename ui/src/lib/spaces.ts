import { isTauri, tauriInvoke } from './platform'

export type Space = {
  id: string
  name: string
  server: string | null
}

const ACTIVE = 'jess.active-space'
const DEFAULT: Space = { id: 'local', name: 'Local Space', server: null }

function readWeb(): Space {
  try {
    const raw = localStorage.getItem(ACTIVE)
    if (!raw) return DEFAULT
    const space = JSON.parse(raw) as Partial<Space>
    if (typeof space.id !== 'string' || typeof space.name !== 'string') return DEFAULT
    return { id: space.id, name: space.name, server: typeof space.server === 'string' ? space.server : null }
  } catch {
    return DEFAULT
  }
}

export async function getSpace(): Promise<Space> {
  if (isTauri) {
    const space = await tauriInvoke<Space | null>('get_space')
    return space ?? DEFAULT
  }
  return readWeb()
}

export async function setSpace(space: Space): Promise<void> {
  if (isTauri) {
    await tauriInvoke('set_space', { space })
    return
  }
  localStorage.setItem(ACTIVE, JSON.stringify(space))
}

export function spaceDbName(space: Space): string {
  return `jess-space-${space.id.replace(/[^a-zA-Z0-9_-]/g, '_')}`
}
