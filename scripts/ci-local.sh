#!/usr/bin/env bash
# Runs the CI workflow's jobs (.github/workflows/ci.yml) on this machine: much faster than
# GitHub's runners. One log per job in target/ci-local/, and a pass/fail summary at the end.
#
#   scripts/ci-local.sh                 # every job
#   scripts/ci-local.sh rust web        # just these (rust simulation yjs-compat web bench linux-app android-app docker)
#
# Differences from GitHub CI (see AGENTS.md for the toolchain):
#   - WebKit e2e runs in Playwright's Ubuntu image (its WebKit doesn't run on Fedora), with a
#     `jess` built in rust:1.93-bookworm (the host binary needs a newer glibc).
#   - linux-app smoke-tests the release binary, the deb's installed layout (extracted, not
#     installed: a binary built here needs a newer glibc than Ubuntu 24.04's) and the AppImage,
#     plus the AppImage's no-sandbox fallback under Xvfb in a container that blocks user
#     namespaces. Installing the deb on stock Ubuntu (AppArmor, apt dependencies) stays on GitHub.
#   - The smoke tests open windows on this display (no Xvfb on the host): don't type into them.
#   - android-app needs the AVD `jess33` (docs/ANDROID.md).
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
out=$root/target/ci-local
mkdir -p "$out"
pnpm() { npx -y pnpm@12.8.1 "$@"; }
export NDK_HOME=${NDK_HOME_OVERRIDE:-$HOME/Android/Sdk/ndk/28.2.13676358}
export JAVA_HOME=${JAVA_HOME_OVERRIDE:-$HOME/android-studio/android-studio/jbr}
export ANDROID_HOME=$HOME/Android/Sdk
export PATH=$ANDROID_HOME/platform-tools:$ANDROID_HOME/emulator:$PATH

# pdfium for the PDF tests: the same build CI fetches.
pdfium_dir=$HOME/.cache/jess/pdfium
export JESS_PDFIUM_LIB=$pdfium_dir/libpdfium.so
fetch_pdfium() {
  [ -f "$JESS_PDFIUM_LIB" ] && return
  mkdir -p "$pdfium_dir"
  local whl=$pdfium_dir/p.whl
  curl -fsSL -o "$whl" https://files.pythonhosted.org/packages/d3/7c/74a2fb48e5b0d2402d9ca64b39074c722d67e9a8a2c58449a843a8c2329a/pypdfium2-5.13.0-py3-none-manylinux_2_17_x86_64.manylinux2014_x86_64.whl
  echo "81df25c1ab4c13ff773102d3cbea1967511d079123b067fc077bd0c4d57d91d8  $whl" | sha256sum -c - >/dev/null
  unzip -q -o -j "$whl" 'pypdfium2_raw/libpdfium.so' -d "$pdfium_dir"
  rm -f "$whl"
}

job_rust() {
  cargo fmt --all -- --check &&
    cargo clippy --workspace --exclude jess-notes-app --all-targets -- -D warnings &&
    cargo test --workspace --exclude jess-notes-app &&
    cargo clippy -p jess-native --features spaces --all-targets -- -D warnings &&
    cargo test -p jess-native --features spaces --test spaces &&
    cargo build -p jess-core-wasm --target wasm32-unknown-unknown --release &&
    CRASH_ITERS=20 cargo test -p jess-server --test crash
}

job_simulation() {
  SIM_SEEDS=${SIM_SEEDS:-10000} cargo test --release -p jess-server --test sim
}

job_yjs_compat() {
  (cd core/tests/yjs-compat && (npm ci || npm install) && node gen.mjs) &&
    cargo test -p jess-core --test yjs_compat &&
    (cd core/tests/yjs-compat && node verify.mjs) &&
    git diff --exit-code core/fixtures
}

job_web() {
  (cd ui && pnpm install --frozen-lockfile && pnpm wasm && pnpm check && pnpm test && pnpm build) &&
    cargo build -p jess-server &&
    (cd ui && pnpm e2e) &&
    # WebKit, in Playwright's Ubuntu image with a jess built for its glibc.
    docker run --rm -v "$root":"$root" -w "$root" -e CARGO_TARGET_DIR="$root/target/bookworm" \
      -v jess-cargo:/usr/local/cargo/registry rust:1.93-bookworm cargo build -q -p jess-server &&
    docker run --rm --ipc=host -v "$root":"$root" -v "$pdfium_dir":"$pdfium_dir" -w "$root/ui" \
      -e JESS_BIN="$root/target/bookworm/debug/jess" -e JESS_PDFIUM_LIB="$JESS_PDFIUM_LIB" -e E2E_WEBKIT=1 \
      mcr.microsoft.com/playwright:v1.56.1-noble npx playwright test --project=webkit
  local rc=$?
  # Files the container wrote as root.
  docker run --rm -v "$root/ui":/w alpine chown -R "$(id -u):$(id -g)" /w/test-results >/dev/null 2>&1
  [ $rc -eq 0 ] || return $rc
  # Minimum engine, Chromium 100.
  local c100=$HOME/.cache/jess/chromium-100
  if [ ! -x "$c100/chrome-linux/chrome" ]; then
    mkdir -p "$c100" && curl -fsSL -o "$c100/c.zip" https://commondatastorage.googleapis.com/chromium-browser-snapshots/Linux_x64/972765/chrome-linux.zip &&
      unzip -q -o "$c100/c.zip" -d "$c100" && rm -f "$c100/c.zip"
  fi
  node ui/e2e/old-chromium.mjs "$c100/chrome-linux/chrome"
}

job_linux_app() {
  (cd ui && pnpm install --frozen-lockfile && pnpm wasm && pnpm build) &&
    cargo clippy -p jess-notes-app --all-targets -- -D warnings &&
    cargo build -p jess-server &&
    cargo build --release -p jess-notes-app --features tauri/custom-protocol &&
    apps/tauri/cef/stage.sh target/release &&
    (cd apps/tauri && ../../ui/node_modules/.bin/tauri build --bundles deb,appimage) &&
    for d in target/release/bundle/deb/*.deb; do apps/tauri/cef/fix-deb.sh "$d" || return 1; done || return 1
  for a in target/release/bundle/appimage/*.AppImage; do apps/tauri/cef/fix-appimage.sh "$a" || return 1; done
  local x
  x=$(mktemp -d)
  (cd "$x" && ar x "$root"/target/release/bundle/deb/*.deb && tar xzf data.tar.gz) &&
    node apps/tauri/e2e/smoke.mjs "$x/usr/bin/jess-notes-app" target/debug/jess &&
    APPIMAGE_EXTRACT_AND_RUN=1 node apps/tauri/e2e/smoke.mjs target/release/bundle/appimage/*.AppImage target/debug/jess
  local rc=$?
  rm -rf "$x"
  [ $rc -eq 0 ] || return $rc
  # The no-sandbox fallback, under Xvfb, where the container blocks user namespaces.
  local h=$out/xhome
  mkdir -p "$h"
  docker build -q -t jess-xvfb -f - "$out" >/dev/null <<'EOF' &&
FROM fedora:44
RUN dnf install -y -q xorg-x11-server-Xvfb xorg-x11-xauth which nodejs gtk3 nss alsa-lib libXcomposite libXdamage libXrandr mesa-libgbm libxkbcommon-x11 cups-libs at-spi2-atk libdrm python3 procps-ng > /dev/null && dnf clean all
EOF
    docker run --rm --ipc=host --shm-size=1g --user "$(id -u):$(id -g)" -e HOME="$h" -e JESS_NO_SANDBOX=1 \
      -v "$h":"$h" -v "$root":"$root" -w "$root" jess-xvfb \
      sh -c "xvfb-run -a -s '-screen 0 1280x900x24' node apps/tauri/e2e/smoke.mjs target/release/jess-notes-app target/debug/jess"
}

job_android_app() {
  (cd ui && pnpm install --frozen-lockfile && pnpm wasm && pnpm build) &&
    cargo build -p jess-server &&
    (cd apps/tauri && ../../ui/node_modules/.bin/tauri android build --debug --target x86_64 --apk) || return 1
  local started=0
  if ! adb get-state >/dev/null 2>&1; then
    (emulator -avd jess33 -no-snapshot -no-audio -no-boot-anim >"$out/emulator.log" 2>&1 &)
    started=1
    adb wait-for-device
    until [ "$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ]; do sleep 3; done
  fi
  adb uninstall app.jessnotes.notes >/dev/null 2>&1
  adb install -r apps/tauri/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk &&
    node apps/tauri/e2e/android-smoke.mjs target/debug/jess
  local rc=$?
  [ $started = 1 ] && adb emu kill >/dev/null 2>&1
  return $rc
}

job_bench() {
  (cd ui && pnpm install --frozen-lockfile && pnpm wasm && pnpm build) && scripts/bench.sh
}

job_docker() {
  docker build -q -t jess-ci-local .
}

all=(rust simulation yjs-compat web bench linux-app android-app docker)
jobs=("$@")
[ ${#jobs[@]} -eq 0 ] && jobs=("${all[@]}")
fetch_pdfium
declare -A result
for j in "${jobs[@]}"; do
  fn=job_${j//-/_}
  if ! declare -F "$fn" >/dev/null; then
    echo "unknown job: $j (jobs: ${all[*]})" >&2
    exit 2
  fi
  t=$(date +%s)
  printf '%-12s ' "$j"
  if "$fn" >"$out/$j.log" 2>&1; then result[$j]=pass; else result[$j]=FAIL; fi
  echo "${result[$j]}  ($(($(date +%s) - t)) s, $out/$j.log)"
done
for j in "${jobs[@]}"; do [ "${result[$j]}" = pass ] || exit 1; done
echo "all passed"
