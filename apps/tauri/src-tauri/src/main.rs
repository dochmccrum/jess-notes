// No console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "linux")]
    if jess_notes_app::cef_helper() {
        return;
    }
    jess_notes_app::run()
}
