// Name helpers mirroring core::names.

export const nfc = (s: string) => s.normalize('NFC')
export const lookupKey = (s: string) => s.normalize('NFC').toLowerCase()

export function splitExt(name: string, folder = false): [string, string] {
  if (folder) return [name, '']
  const i = name.lastIndexOf('.')
  return i > 0 ? [name.slice(0, i), name.slice(i)] : [name, '']
}

/** Obsidian's rules for names typed inside Jess. Returns an error message or null. */
export function validateName(name: string): string | null {
  if (!name.trim()) return 'Name cannot be empty'
  const bad = name.match(/[*"\\/<>:|?#^[\]]/)
  if (bad) return `Names cannot contain “${bad[0]}”`
  if (/[\u0000-\u001f]/.test(name)) return 'Names cannot contain control characters'
  if (name.startsWith('.')) return 'Names cannot start with a dot'
  if (/[. ]$/.test(name)) return 'Names cannot end with a dot or space'
  return null
}

/** Smallest free `Name n.ext` among `taken` (NFC, case-sensitive like the server). */
export function freeName(name: string, taken: Set<string>, folder = false): string {
  if (!taken.has(nfc(name))) return name
  const [stem, ext] = splitExt(name, folder)
  for (let n = 1; ; n++) {
    const c = `${stem} ${n}${ext}`
    if (!taken.has(nfc(c))) return c
  }
}

export const isMarkdownName = (n: string) => /\.md$/i.test(n)
export const isImageName = (n: string) => /\.(png|jpe?g|gif|webp|svg|heic|heif|bmp|avif|tiff?)$/i.test(n)
export const isPdfName = (n: string) => /\.pdf$/i.test(n)
export const displayName = (n: string) => (isMarkdownName(n) ? n.slice(0, -3) : n)
