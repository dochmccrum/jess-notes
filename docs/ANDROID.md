# Android

The Android app is the Tauri crate in `apps/tauri` built for Android. Design notes: DESIGN §11.5,
§16 and §22 (items 48–55).

## Toolchain

- Android SDK (`ANDROID_HOME`, usually `~/Android/Sdk`) with platform 37 and build tools.
- NDK r28: `sdkmanager "ndk;28.2.13676358"`, then `NDK_HOME=$ANDROID_HOME/ndk/28.2.13676358`.
- A full JDK 21 (Gradle needs `javac`, and a JRE isn't enough). Android Studio's bundled one works:
  `JAVA_HOME=~/android-studio/android-studio/jbr` (Fedora's `java-25-openjdk-headless` has no
  compiler).
- Rust targets: `rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`.
- The UI built first: `cd ui && pnpm install && pnpm wasm && pnpm build`.

## Building

From `apps/tauri` (the Tauri CLI lives in `ui/node_modules`):

```sh
TAURI=../../ui/node_modules/.bin/tauri
$TAURI android build --debug --target x86_64 --apk   # emulator; WebView debugging on
$TAURI android build --apk --split-per-abi           # release APKs, one per ABI (sideloading)
$TAURI android build --aab                           # release bundle (Play Store)
```

Outputs are in `src-tauri/gen/android/app/build/outputs/`. Install the arm64 APK on a phone:
`adb install -r apk/arm64/release/app-arm64-release.apk`. A plain `tauri android build` also
makes a universal APK with all four ABIs (~100 MB). Don't ship that one.

Sizes (release, 2026-09-30): arm64 APK 27.7 MB, armv7 22 MB, AAB 41 MB (Play serves one ABI).
The native library is ~20 MB per ABI. Size work (LTO, stripping) is in phase 7.

## Signing

Release builds are unsigned unless `src-tauri/gen/android/keystore.properties` exists (git-ignored):

```properties
storeFile=/absolute/path/to/jess-release.jks
storePassword=…
keyAlias=jess
keyPassword=…
```

Create the key once, then keep it (and its passwords) somewhere safe. **Android refuses updates
signed with a different key**, so losing it means uninstalling (and losing local data) on every
device:

```sh
keytool -genkeypair -keystore jess-release.jks -alias jess -keyalg RSA -keysize 4096 -validity 10000
```

## Testing on a device or emulator

`apps/tauri/e2e/android-smoke.mjs` drives the real app over the WebView's DevTools socket
(Playwright `_android`, through adb). It needs a **debug** APK installed, one device attached and
`target/debug/jess` built. It covers sign-in, typing with the soft keyboard up (the caret must stay
visible), paste of an image and a PDF, the PDF viewer, search, the back gesture, foreground
catch-up of 200 remote notes, and cold start:

```sh
adb install -r apps/tauri/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
node apps/tauri/e2e/android-smoke.mjs target/debug/jess
```

The server runs on the host and is reached through `adb reverse`. CI runs the same script on an
API 33 emulator.

### Spaces (DESIGN §24)

A fresh install opens on the spaces welcome screen; the smoke test creates a local space (the
server embedded in the app), then adds the test server as a remote space. Adding, switching and
erasing restart the app through `RestartActivity` (a separate `:restart` process starts
`MainActivity` again), so the test re-attaches to the new process's WebView after each.
Scanning a pairing QR code needs a real camera: on a phone, open Settings → Pair a device on a
signed-in device (any platform) and scan it from Add a space → On a Jess server.

### 120 Hz (DESIGN §23.4)

The app asks for the display's fastest mode, and the smoke test logs the WebView's frame rate.
Emulators only offer 60 Hz, so check on a real 120 Hz phone (with any battery saver off, which
caps the rate):

```sh
SMOKE_HZ=120 node apps/tauri/e2e/android-smoke.mjs target/debug/jess
```

It fails if `requestAnimationFrame` runs below 114 fps. Scrolling and typing smoothness follow
the desktop numbers (the same Chromium engine), but a mid-range phone's CPU is 2–4× slower: if
it drops frames, note the interaction in docs/PHASES.md.

### Older WebViews

The minimum is **Chromium 100** (DESIGN §22 item 53). Real devices update "Android System WebView"
from the Play Store; an older one gets a message asking for the update (`ui/public/compat.js`)
instead of a blank page. What was checked:

| Engine | Result |
| --- | --- |
| Android 13 emulator, WebView 109 | full smoke test passes |
| Chromium 100 (desktop build, same bundle) | sign in, edit, sync, reload: `ui/e2e/old-chromium.mjs`, in CI |
| Android 10 emulator, WebView 74 (never updated) | "needs a newer Android System WebView" message |

Chromium only publishes ARM builds of old Android WebViews, and those don't run on x86 emulators,
so the Chromium 100 check uses the desktop build of the same engine.

## Measurements (x86_64 emulator on the dev machine, 2026-09-30)

- Foreground catch-up, 200 remote notes: applied 50–100 ms after the app is back in front.
- PDF first page: 190–230 ms.
- Keyboard up: the WebView shrinks (915 → 530 px) and the caret stays visible.
- Cold start, **debug** build: launch → note visible 0.87–1.6 s (WebView navigation → note
  270–640 ms). Release build: first frame 113–219 ms (`am start -W`). Release builds can't attach
  DevTools, so their note-visible time isn't measured here. The SPEC target (<500 ms) needs a
  release measurement on a real mid-range phone, and pre-warming if it misses. That work is in
  phase 7.
