//! Yjs ↔ yrs compatibility (DESIGN §17.4). `fixtures/yjs-updates.json` is produced by
//! `tests/yjs-compat/gen.mjs` (real Yjs); this test applies it with yrs. It also (re)writes
//! `fixtures/yrs-updates.json`, which `tests/yjs-compat/verify.mjs` applies with Yjs.

use jess_core::doc as ydoc;
use jess_core::rewrite::Rewrite;
use jess_core::Id;
use serde_json::{json, Value};

fn fixtures() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

#[test]
fn yjs_updates_apply_in_yrs() {
    let cases: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures().join("yjs-updates.json")).unwrap(),
    )
    .unwrap();
    for c in cases.as_array().unwrap() {
        let d = ydoc::new_doc(999);
        for u in c["updates"].as_array().unwrap() {
            ydoc::apply(&d, &hex::decode(u.as_str().unwrap()).unwrap()).unwrap();
        }
        assert_eq!(
            ydoc::text(&d),
            c["text"].as_str().unwrap(),
            "case {}",
            c["name"]
        );
        assert!(!ydoc::has_pending(&d));
        // UTF-16 length agrees with JS string length.
        assert_eq!(
            ydoc::len16(&d) as usize,
            c["text"].as_str().unwrap().encode_utf16().count()
        );
    }
}

#[test]
fn yrs_updates_for_yjs() {
    let mut cases = vec![];
    // Edits at UTF-16 offsets around astral characters and CRLF.
    let d = ydoc::new_doc(4001);
    let mut ups = vec![ydoc::insert(&d, 0, "\u{feff}😀 [[Old]]\r\n日本 ")];
    ups.push(ydoc::insert(&d, 3, "é"));
    ups.push(ydoc::remove(&d, 1, 2)); // remove 😀
    let text = ydoc::text(&d);
    cases.push(json!({ "name": "utf16-edits", "updates": ups.iter().map(hex::encode).collect::<Vec<_>>(), "text": text }));
    // A server-style link rewrite (replace only the target substring).
    let t = ydoc::text(&d);
    let ex = jess_core::links::extract(&t);
    let l = &ex.links[0];
    let rw = ydoc::apply_rewrites(
        &d,
        &[Rewrite {
            src: Id::default(),
            start16: l.target_range.0,
            end16: l.target_range.1,
            old: "Old".into(),
            new: "New name".into(),
        }],
    );
    ups.push(rw);
    cases.push(json!({ "name": "rewrite", "updates": ups.iter().map(hex::encode).collect::<Vec<_>>(), "text": ydoc::text(&d), "rewrite_check": "[[New name]]" }));
    // Merged + state-as-update.
    cases.push(json!({ "name": "merged", "updates": [hex::encode(ydoc::merge(&ups).unwrap())], "text": ydoc::text(&d) }));
    cases.push(json!({ "name": "state", "updates": [hex::encode(ydoc::encode_state(&d))], "text": ydoc::text(&d) }));
    let out = serde_json::to_string_pretty(&cases).unwrap() + "\n";
    let p = fixtures().join("yrs-updates.json");
    if std::fs::read_to_string(&p).ok().as_deref() != Some(out.as_str()) {
        std::fs::write(&p, out).unwrap();
    }
    assert_eq!(ydoc::text(&d), "\u{feff}é [[New name]]\r\n日本 ");
}
