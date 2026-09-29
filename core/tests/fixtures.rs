//! Runs the shared conformance fixtures in `core/fixtures/`.

use jess_core::links::{extract, parse_pdf_subpath, split_alt_size, Syntax};
use jess_core::resolve::ResolveIndex;
use jess_core::Id;
use serde_json::Value;

fn load(name: &str) -> Value {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn slice16(text: &str, r: (u32, u32)) -> String {
    let u: Vec<u16> = text.encode_utf16().collect();
    String::from_utf16(&u[r.0 as usize..r.1 as usize]).unwrap()
}

#[test]
fn links() {
    let mut failures = vec![];
    for case in load("links.json").as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let got: Vec<Value> = extract(text)
            .links
            .iter()
            .map(|l| {
                let mut o = serde_json::json!({
                    "syntax": if l.syntax == Syntax::Wiki { "wiki" } else { "markdown" },
                    "embed": l.embed,
                    "target": l.target,
                    "span": slice16(text, l.range),
                    "target_span": slice16(text, l.target_range),
                });
                if let Some(s) = &l.subpath {
                    o["subpath"] = s.clone().into();
                }
                if let Some(d) = &l.display {
                    o["display"] = d.clone().into();
                }
                o
            })
            .collect();
        let want = case["links"].as_array().unwrap();
        // Order-insensitive (nested links are reported by position).
        let mut g = got.clone();
        let mut w = want.clone();
        g.sort_by_key(|v| v.to_string());
        w.sort_by_key(|v| v.to_string());
        if g != w {
            failures.push(format!(
                "{text:?}\n  want {}\n  got  {}",
                serde_json::to_string(want).unwrap(),
                serde_json::to_string(&got).unwrap()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn tags() {
    let mut failures = vec![];
    for case in load("tags.json").as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let got: Vec<String> = extract(text).tags.into_iter().map(|t| t.name).collect();
        let want: Vec<String> = case["tags"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        if got != want {
            failures.push(format!("{text:?}: want {want:?} got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn math() {
    let mut failures = vec![];
    for case in load("math.json").as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let got: Vec<(String, bool)> = extract(text)
            .math
            .iter()
            .map(|m| (slice16(text, m.range), m.display))
            .collect();
        let want: Vec<(String, bool)> = case["math"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["span"].as_str().unwrap().to_string(),
                    m["display"].as_bool().unwrap(),
                )
            })
            .collect();
        if got != want {
            failures.push(format!("{text:?}: want {want:?} got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn embeds() {
    let f = load("embeds.json");
    for c in f["sizes"].as_array().unwrap() {
        let (alt, size) = split_alt_size(c["display"].as_str().unwrap());
        assert_eq!(alt.as_deref(), c["alt"].as_str(), "{c}");
        let want = c["size"].as_array().map(|a| {
            (
                a[0].as_u64().unwrap() as u32,
                a[1].as_u64().map(|v| v as u32),
            )
        });
        assert_eq!(size, want, "{c}");
    }
    for c in f["pdf"].as_array().unwrap() {
        let p = parse_pdf_subpath(c["subpath"].as_str().unwrap());
        assert_eq!(p.page, c["page"].as_u64().map(|v| v as u32), "{c}");
        assert_eq!(p.height, c["height"].as_u64().map(|v| v as u32), "{c}");
    }
}

#[test]
fn resolution() {
    let f = load("resolution.json");
    let mut ix = ResolveIndex::new();
    let paths: Vec<String> = f["vault"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    for (i, p) in paths.iter().enumerate() {
        ix.insert(Id([i as u8 + 1; 16]), p);
    }
    let mut failures = vec![];
    for c in f["cases"].as_array().unwrap() {
        let from = c["from"].as_str().unwrap();
        let folder = from.rsplit_once('/').map(|(f, _)| f).unwrap_or("");
        let syntax = if c["syntax"] == "wiki" {
            Syntax::Wiki
        } else {
            Syntax::Markdown
        };
        let got = ix
            .resolve(c["target"].as_str().unwrap(), syntax, folder)
            .map(|r| paths[r.id.0[0] as usize - 1].clone());
        let want = c["expect"].as_str().map(String::from);
        if got != want {
            failures.push(format!("{c}: got {got:?}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn sanitisation() {
    use jess_core::apply::{apply_meta, Ctx, Mode, Tx};
    use jess_core::hlc::Hlc;
    use jess_core::ops::MetaOp;
    use jess_core::projection::{project, sanitize_segment, Options, Profile};
    use jess_core::state::MetaState;
    let f = load("sanitisation.json");
    for c in f["segments"].as_array().unwrap() {
        assert_eq!(
            sanitize_segment(c["name"].as_str().unwrap()),
            c["portable"].as_str().unwrap(),
            "{c}"
        );
    }
    // Collisions: the sibling with the smaller entry id keeps its name (ids follow list order).
    for c in f["collisions"].as_array().unwrap() {
        let mut st = MetaState::new();
        let folders = c["folders"].as_bool().unwrap_or(false);
        let names: Vec<&str> = c["siblings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for (i, n) in names.iter().enumerate() {
            let op = MetaOp::Create {
                id: Id([i as u8 + 1; 16]),
                kind: if folders { "folder" } else { "markdown" }.into(),
                parent: None,
                name: n.to_string(),
                tree_visible: true,
                blob: None,
                blob_info: None,
                created_at: None,
                modified_at: None,
                props: vec![],
            };
            apply_meta(
                &mut st,
                &mut Tx::new(),
                &op,
                &Ctx {
                    hlc: Hlc::new(1 + i as u64, 0, 1),
                    known_seq: 0,
                    replica: 1,
                    op_id: i as u64,
                    mode: Mode::Server,
                    now: 0,
                },
            )
            .unwrap();
        }
        let p = project(
            &st,
            Options {
                profile: Profile::Portable,
                include_trash: false,
            },
        );
        let by = p.by_id();
        let got: Vec<String> = (0..names.len())
            .map(|i| by[&Id([i as u8 + 1; 16])].path.clone())
            .collect();
        let want: Vec<String> = c["portable"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, want, "{c}");
    }
}
