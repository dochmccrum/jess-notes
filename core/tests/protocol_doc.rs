//! Keeps docs/PROTOCOL.md honest: its golden encodings must match what the code produces.
//! Run with `UPDATE_PROTOCOL_DOC=1` to print the current values.

use jess_core::hlc::Hlc;
use jess_core::ops::{AckResult, MetaOp, Op, OpBody, Reject};
use jess_core::proto::{encode, ClientMsg, Hello, ServerMsg};
use jess_core::Id;

fn goldens() -> Vec<(&'static str, String)> {
    let hello = ClientMsg::Hello(Hello {
        proto: 1,
        vault_id: None,
        replica_id: 7,
        token: "t".into(),
        cursor: 42,
        app_version: "1".into(),
    });
    let push = ClientMsg::Push {
        ops: vec![Op {
            op_id: 1,
            hlc: Hlc::new(1000, 0, 7),
            known_seq: 42,
            group: 0,
            body: OpBody::Meta(MetaOp::SetName {
                id: Id([1; 16]),
                name: "a.md".into(),
            }),
        }],
    };
    let ack = ServerMsg::Ack {
        results: vec![
            (1, AckResult::Applied(43)),
            (2, AckResult::Rejected(Reject::Cycle)),
        ],
    };
    vec![
        ("hello", hex::encode(encode(&hello))),
        ("push", hex::encode(encode(&push))),
        ("ack", hex::encode(encode(&ack))),
    ]
}

#[test]
fn protocol_doc_goldens() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/PROTOCOL.md"),
    )
    .unwrap();
    for (name, hex) in goldens() {
        if std::env::var("UPDATE_PROTOCOL_DOC").is_ok() {
            println!("{name}: {hex}");
            continue;
        }
        assert!(
            doc.contains(&format!("`{name}`: `{hex}`")),
            "docs/PROTOCOL.md golden for {name} is stale; expected `{hex}`"
        );
    }
}
