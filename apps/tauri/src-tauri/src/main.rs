// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // A local space's server derives attachment variants by re-executing this binary as
    // `<exe> derive <kind> <input> <outdir>` (DESIGN §24.1), like `jess derive`.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("derive") {
        if let Err(e) = jess_native::spaces::derive_cli(&args[1..]) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    // Relaunched into another space (lib.rs `relaunch`): let the previous instance finish first.
    // CEF won't share its profile (it hands this launch to the running instance and stops).
    #[cfg(target_os = "linux")]
    let relaunched = match std::env::var("JESS_WAIT_FOR_PID") {
        Ok(pid) => {
            std::env::remove_var("JESS_WAIT_FOR_PID");
            wait_for_previous_instance(&pid);
            true
        }
        Err(_) => false,
    };
    #[cfg(target_os = "linux")]
    jess_notes_app::ensure_no_sandbox_arg();
    #[cfg(target_os = "linux")]
    if jess_notes_app::cef_helper() {
        return;
    }
    #[cfg(target_os = "linux")]
    if relaunched {
        wait_for_x_display();
    }
    jess_notes_app::run()
}

/// Waits (up to 15 s) for the previous instance: its process, then the CEF profile's
/// `SingletonLock` (a symlink to `host-pid`, removed on exit), whose holder may be the same
/// process or one of its helpers. Chromium makes its processes non-dumpable, so this goes by the
/// lock rather than by looking for processes running this executable.
#[cfg(target_os = "linux")]
fn wait_for_previous_instance(pid: &str) {
    // Running, and not a zombie its parent hasn't reaped yet.
    fn alive(pid: &str) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| {
                !s.rsplit(')')
                    .next()
                    .unwrap_or("")
                    .trim_start()
                    .starts_with('Z')
            })
            .unwrap_or(false)
    }
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".cache")));
    let lock = cache.map(|c| {
        c.join(jess_notes_app::APP_ID)
            .join("cef")
            .join("SingletonLock")
    });
    let lock_held = || {
        let Some(l) = &lock else { return false };
        match std::fs::read_link(l) {
            Ok(t) => t
                .to_string_lossy()
                .rsplit('-')
                .next()
                .is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit()) && alive(p)),
            Err(_) => false,
        }
    };
    let t = std::time::Instant::now();
    while (alive(pid) || lock_held()) && t.elapsed() < std::time::Duration::from_secs(15) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// After a relaunch, waits (up to 5 s) until the X display accepts a connection: the window can't
/// be created otherwise, and CI saw a relaunched instance fail exactly there. Logs why if it can't.
#[cfg(target_os = "linux")]
fn wait_for_x_display() {
    if std::env::var_os("DISPLAY").is_none() {
        return;
    }
    let Ok(xlib) = x11_dl::xlib::Xlib::open() else {
        eprintln!("jess: libX11 couldn't be loaded");
        return;
    };
    let t = std::time::Instant::now();
    loop {
        // SAFETY: Xlib calls with a null display name (DISPLAY) and the display they return.
        let d = unsafe { (xlib.XOpenDisplay)(std::ptr::null()) };
        if !d.is_null() {
            unsafe { (xlib.XCloseDisplay)(d) };
            if t.elapsed().as_millis() > 0 {
                eprintln!("jess: X display reachable after {:?}", t.elapsed());
            }
            return;
        }
        if t.elapsed() > std::time::Duration::from_secs(5) {
            eprintln!(
                "jess: can't open X display {:?} (XAUTHORITY {:?})",
                std::env::var_os("DISPLAY"),
                std::env::var_os("XAUTHORITY")
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
