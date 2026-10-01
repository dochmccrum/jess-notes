fn main() {
    // Desktop Linux ships libcef.so (and CEF's resources) in the app's own directory.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/jess-notes");
        // Keep our bundled SQLite private. The linker otherwise exports its `sqlite3_*` symbols, and
        // CEF's in-process NSS then loads the system libsqlite3, whose calls bind to our copy and
        // crash (two SQLite builds mixing their structs).
        println!("cargo:rustc-link-arg-bins=-Wl,--exclude-libs,ALL");
    }
    tauri_build::build()
}
