// Command and keybinding registries (DESIGN §11.6). Menus, the palette and keys all call
// commands by id; drag and drop later is just another caller.

export interface CommandContext {
  target?: string // entry id the command applies to (context menu, tree focus)
}

export interface Command {
  id: string
  title: string
  run(ctx: CommandContext): void | Promise<void>
  when?(ctx: CommandContext): boolean
  hidden?: boolean
}

const commands = new Map<string, Command>()
const bindings = new Map<string, string>() // normalised key → command id
const defaults = new Map<string, string>()

export const isMac = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent)

export function register(c: Command) {
  commands.set(c.id, c)
}

export function all(): Command[] {
  return [...commands.values()].filter((c) => !c.hidden)
}

export function get(id: string) {
  return commands.get(id)
}

export async function run(id: string, ctx: CommandContext = {}) {
  const c = commands.get(id)
  if (!c || (c.when && !c.when(ctx))) return false
  await c.run(ctx)
  return true
}

/** `Mod-\\`, `Mod-o`, `Shift-Mod-p`, `F2`… (`Mod` = Cmd on Apple, Ctrl elsewhere). */
export function normalise(key: string): string {
  const parts = key.split('-')
  let k = parts.pop()!
  if (k === '') k = '-'
  const mods = new Set(parts.map((p) => (p === 'Mod' ? (isMac ? 'Meta' : 'Ctrl') : p === 'Cmd' ? 'Meta' : p)))
  const order = ['Alt', 'Ctrl', 'Meta', 'Shift'].filter((m) => mods.has(m))
  return [...order, k.length === 1 ? k.toLowerCase() : k].join('-')
}

export function bind(key: string, id: string, isDefault = true) {
  bindings.set(normalise(key), id)
  if (isDefault) defaults.set(normalise(key), id)
}

export function applyOverrides(o: Record<string, string>) {
  for (const [k, id] of Object.entries(o)) {
    for (const [bk, bid] of [...bindings]) if (bid === id) bindings.delete(bk)
    bindings.set(normalise(k), id)
  }
}

export function eventKey(e: KeyboardEvent): string {
  const mods = []
  if (e.altKey) mods.push('Alt')
  if (e.ctrlKey) mods.push('Ctrl')
  if (e.metaKey) mods.push('Meta')
  if (e.shiftKey) mods.push('Shift')
  const k = e.key.length === 1 ? e.key.toLowerCase() : e.key
  return [...mods, k].join('-')
}

export function keyFor(id: string): string | undefined {
  for (const [k, v] of bindings) if (v === id) return k.replace('Meta', '⌘').replace('Ctrl', 'Ctrl').replace(/-/g, '+')
  return undefined
}

/** Handles a keydown; returns true if a command ran. */
export function handleKey(e: KeyboardEvent, ctx: CommandContext = {}): boolean {
  const id = bindings.get(eventKey(e))
  if (!id) return false
  const c = commands.get(id)
  if (!c || (c.when && !c.when(ctx))) return false
  e.preventDefault()
  void c.run(ctx)
  return true
}

export function bindingsFor(): [string, string][] {
  return [...bindings]
}
