// Web vs Tauri. The UI bundle is the same; a few host services differ (DESIGN §2): where the
// token and boot record live, how auth requests reach the server (no CORS through Rust), and
// how <img> reaches blob bytes.

export const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
export const isAndroid = typeof navigator !== 'undefined' && /Android/.test(navigator.userAgent)

/** The `jess-blob` scheme URL (Android/Windows WebViews use the http form). */
export function nativeBlobUrl(hash: string, variant: string): string {
  return isAndroid ? `http://jess-blob.localhost/${hash}/${variant}` : `jess-blob://localhost/${hash}/${variant}`
}

type Internals = { invoke: <T>(cmd: string, args?: Record<string, unknown>, options?: unknown) => Promise<T> }

/** A Tauri command, straight through the IPC bridge (what `@tauri-apps/api/core`'s `invoke`
 *  calls), so boot needn't load that module before its first command. */
export function tauriInvoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__.invoke<T>(cmd, args ?? {})
}
