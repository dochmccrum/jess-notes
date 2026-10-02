fn main() {
    // Desktop Linux ships libcef.so with CEF's resources beside it: /usr/lib/jess-notes in the deb,
    // the AppImage's usr/lib (linuxdeploy rewrites the runpath to `$ORIGIN/../lib` and copies
    // whatever libcef.so it resolves there, so CEF's resources must be there too).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN:$ORIGIN/../lib/jess-notes:$ORIGIN/../lib"
        );
        // Keep our bundled SQLite private. The linker otherwise exports its `sqlite3_*` symbols, and
        // CEF's in-process NSS then loads the system libsqlite3, whose calls bind to our copy and
        // crash (two SQLite builds mixing their structs).
        println!("cargo:rustc-link-arg-bins=-Wl,--exclude-libs,ALL");
    }
    tauri_build::build()
}
