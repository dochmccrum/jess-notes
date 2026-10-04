//! Server benchmark (DESIGN §18): five clients typing, apply + commit time per push on a durable
//! database on disk. Ignored by default; `scripts/bench.sh` runs it in release mode:
//!
//!   BENCH_OUT=results.jsonl cargo test --release -p jess-server --test bench -- --ignored

use jess_core::doc as ydoc;
use jess_core::hlc::Hlc;
use jess_core::model::KIND_MARKDOWN;
use jess_core::ops::{AckResult, MetaOp, Op, OpBody};
use jess_core::Id;
use jess_server::db;
use jess_server::engine::{Engine, EngineConfig};
use std::time::Instant;

fn record(key: &str, value: f64, unit: &str) {
    println!("BENCH {key} = {value:.2} {unit}");
    if let Ok(p) = std::env::var("BENCH_OUT") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap();
        writeln!(
            f,
            "{{\"key\":\"{key}\",\"value\":{:.2},\"unit\":\"{unit}\"}}",
            value
        )
        .unwrap();
    }
}

#[test]
#[ignore]
fn five_clients_typing() {
    // On disk (not a tmpfs /tmp), with the server's real durability: every commit is fsync'd.
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let conn = db::open(&dir.path().join("jess.db"), true).unwrap();
    let mut e = Engine::open(conn, EngineConfig::default(), 11).unwrap();
    let mut op = 0u64;
    let mut next = |replica: u64, known: u64, body: OpBody| {
        op += 1;
        Op {
            op_id: op,
            hlc: Hlc::new(1_000_000 + op, 0, replica),
            known_seq: known,
            group: 0,
            body,
        }
    };
    const CLIENTS: u64 = 5;
    let prose = "Some notes with a [[link]], a #tag and $x^2$ maths. ".repeat(80);
    let mut docs = vec![];
    for c in 0..CLIENTS {
        let id = Id([c as u8 + 1; 16]);
        let create = next(
            c + 1,
            e.head,
            OpBody::Meta(MetaOp::Create {
                id,
                kind: KIND_MARKDOWN.into(),
                parent: None,
                name: format!("Typing {c}.md"),
                tree_visible: true,
                blob: None,
                blob_info: None,
                created_at: None,
                modified_at: None,
                props: vec![],
            }),
        );
        e.push(c + 1, None, &[create], 2_000_000).unwrap();
        let d = ydoc::new_doc(100 + c);
        let u = ydoc::insert(&d, 0, &prose);
        let w = next(
            c + 1,
            e.head,
            OpBody::Doc {
                entry: id,
                slot: "body".into(),
                update: u,
            },
        );
        e.push(c + 1, None, &[w], 2_000_000).unwrap();
        docs.push((id, d));
    }
    // Round robin: each push is one client's coalesced keystrokes (a word's worth).
    let mut times = vec![];
    for round in 0..300 {
        for (c, (id, d)) in docs.iter().enumerate() {
            let at = ydoc::len16(d);
            let u = ydoc::insert(
                d,
                at,
                if round % 7 == 0 {
                    " [[Typing 1]] "
                } else {
                    "word "
                },
            );
            let o = next(
                c as u64 + 1,
                e.head,
                OpBody::Doc {
                    entry: *id,
                    slot: "body".into(),
                    update: u,
                },
            );
            let t = Instant::now();
            let r = e.push(c as u64 + 1, None, &[o], 2_000_000 + round).unwrap();
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(matches!(r[0].1, AckResult::Applied(_)), "{r:?}");
        }
    }
    times.sort_by(f64::total_cmp);
    let q = |p: f64| times[((times.len() as f64 * p) as usize).min(times.len() - 1)];
    record("server_apply_commit_p50_ms", q(0.5), "ms");
    record("server_apply_commit_p99_ms", q(0.99), "ms");
}
