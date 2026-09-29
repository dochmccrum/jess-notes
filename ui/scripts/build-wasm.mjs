// Builds jess-core-wasm and generates the JS glue into src/wasm/ (git-ignored).
import { execFileSync } from 'node:child_process'
import { mkdirSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..')
const run = (cmd, args) => execFileSync(cmd, args, { cwd: root, stdio: 'inherit' })
run('cargo', ['build', '-p', 'jess-core-wasm', '--target', 'wasm32-unknown-unknown', '--profile', 'wasm-release'])
mkdirSync(join(root, 'ui/src/wasm'), { recursive: true })
run('wasm-bindgen', ['--target', 'web', '--out-dir', 'ui/src/wasm', '--out-name', 'core', 'target/wasm32-unknown-unknown/wasm-release/jess_core_wasm.wasm'])
try {
  run('wasm-opt', ['-Oz', '--enable-bulk-memory', '--enable-nontrapping-float-to-int', '-o', 'ui/src/wasm/core_bg.wasm', 'ui/src/wasm/core_bg.wasm'])
} catch {
  console.warn('wasm-opt not found: skipping size optimisation')
}
