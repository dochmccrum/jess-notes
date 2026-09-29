// Device settings (DESIGN §3.3): never synced, stored locally.

export type SidebarMode = 'pinned' | 'shortcut' | 'hover'
export type Theme = 'system' | 'light' | 'dark'

export interface DeviceSettings {
  sidebarMode: SidebarMode
  sidebarWidth: number
  sidebarOpen: boolean
  expanded: string[]
  theme: Theme
  showAllAttachments: boolean
  keybindings: Record<string, string>
  lastNote: string | null
  rightPanel: boolean
  offlineAttachments: 'everything' | 'on-demand'
}

const KEY = 'jess.device'

export function defaults(): DeviceSettings {
  const coarse = typeof matchMedia !== 'undefined' && matchMedia('(pointer: coarse)').matches
  return {
    sidebarMode: 'pinned',
    sidebarWidth: 280,
    sidebarOpen: !coarse,
    expanded: [],
    theme: 'system',
    showAllAttachments: false,
    keybindings: {},
    lastNote: null,
    rightPanel: false,
    offlineAttachments: coarse ? 'on-demand' : 'everything',
  }
}

export function loadDevice(): DeviceSettings {
  try {
    return { ...defaults(), ...JSON.parse(localStorage.getItem(KEY) ?? '{}') }
  } catch {
    return defaults()
  }
}

let timer: ReturnType<typeof setTimeout> | undefined
export function saveDevice(s: DeviceSettings) {
  clearTimeout(timer)
  timer = setTimeout(() => {
    try {
      localStorage.setItem(KEY, JSON.stringify(s))
    } catch {
      /* private mode */
    }
  }, 150)
}
