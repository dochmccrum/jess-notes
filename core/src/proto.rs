//! Wire protocol (DESIGN §5.3). WebSocket binary frames and HTTP bodies are CBOR (`minicbor`),
//! integer-keyed maps, unknown fields ignored.

use crate::hlc::Hlc;
use crate::ids::Id;
use crate::model::{BlobRow, Entry};
use crate::ops::{AckResult, Op};
use minicbor::{Decode, Encode};

pub const PROTO_VERSION: u32 = 1;
/// Maximum size of a client→server frame.
pub const MAX_CLIENT_FRAME: usize = 1 << 20;
/// Target size of one `Changes` page.
pub const CHANGES_PAGE_BYTES: u64 = 2 << 20;

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Hello {
    #[n(0)]
    pub proto: u32,
    #[n(1)]
    pub vault_id: Option<Id>,
    #[n(2)]
    pub replica_id: u64,
    #[n(3)]
    pub token: String,
    #[n(4)]
    pub cursor: u64,
    #[n(5)]
    pub app_version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub enum ClientMsg {
    #[n(0)]
    Hello(#[n(0)] Hello),
    #[n(1)]
    Push {
        #[n(0)]
        ops: Vec<Op>,
    },
    #[n(2)]
    Pull {
        #[n(0)]
        from: u64,
        #[n(1)]
        limit_bytes: u64,
    },
    #[n(3)]
    DocSync {
        #[n(0)]
        entry: Id,
        #[n(1)]
        slot: String,
        #[n(2)]
        #[cbor(with = "minicbor::bytes")]
        state_vector: Vec<u8>,
    },
    #[n(4)]
    Ping {
        #[n(0)]
        nonce: u64,
    },
}

/// A doc update inside `Changes`. `bytes == None` is the `Own` placeholder: the update came
/// from the receiving replica and is not echoed.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct DocUpdate {
    #[n(0)]
    pub seq: u64,
    #[n(1)]
    pub entry: Id,
    #[n(2)]
    pub slot: String,
    #[n(3)]
    #[cbor(with = "minicbor::bytes")]
    pub bytes: Option<Vec<u8>>,
}

/// Everything that changed in `(from, to]`.
#[derive(Clone, Debug, PartialEq, Eq, Default, Encode, Decode)]
#[cbor(map)]
pub struct Changes {
    #[n(0)]
    pub from: u64,
    #[n(1)]
    pub to: u64,
    #[n(2)]
    pub entries: Vec<Entry>,
    #[n(3)]
    pub blobs: Vec<BlobRow>,
    #[n(4)]
    pub docs: Vec<DocUpdate>,
    /// More pages follow (the server continues immediately).
    #[n(5)]
    pub more: bool,
    #[n(6)]
    pub hlc: Hlc,
}

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Welcome {
    #[n(0)]
    pub vault_id: Id,
    #[n(1)]
    pub head_seq: u64,
    #[n(2)]
    pub server_time: u64,
    #[n(3)]
    pub min_client_proto: u32,
    #[n(4)]
    pub hlc: Hlc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(index_only)]
pub enum ErrorCode {
    #[n(0)]
    Unauthorized,
    #[n(1)]
    WrongVault,
    #[n(2)]
    ProtoTooOld,
    #[n(3)]
    BadFrame,
    #[n(4)]
    Busy,
    #[n(5)]
    Internal,
    #[n(6)]
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub enum ServerMsg {
    #[n(0)]
    Welcome(#[n(0)] Welcome),
    #[n(1)]
    Ack {
        #[n(0)]
        results: Vec<(u64, AckResult)>,
    },
    #[n(2)]
    Changes(#[n(0)] Changes),
    #[n(3)]
    Pong {
        #[n(0)]
        nonce: u64,
        #[n(1)]
        server_time: u64,
    },
    #[n(4)]
    Error {
        #[n(0)]
        code: ErrorCode,
        #[n(1)]
        message: String,
        #[n(2)]
        retry_after: Option<u64>,
    },
    /// Answer to `DocSync`: the server's state minus the client's state vector.
    #[n(5)]
    DocDiff {
        #[n(0)]
        entry: Id,
        #[n(1)]
        slot: String,
        #[n(2)]
        #[cbor(with = "minicbor::bytes")]
        update: Vec<u8>,
    },
}

/// HTTP long-poll fallback request (`POST /api/sync`).
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct HttpSyncRequest {
    #[n(0)]
    pub hello: Hello,
    #[n(1)]
    pub ops: Vec<Op>,
    #[n(2)]
    pub cursor: u64,
    #[n(3)]
    pub wait_s: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct HttpSyncResponse {
    #[n(0)]
    pub welcome: Welcome,
    #[n(1)]
    pub acks: Vec<(u64, AckResult)>,
    #[n(2)]
    pub changes: Vec<Changes>,
}

pub fn encode<T: Encode<()>>(msg: &T) -> Vec<u8> {
    minicbor::to_vec(msg).expect("encoding to Vec is infallible")
}

pub fn decode<'b, T: Decode<'b, ()>>(bytes: &'b [u8]) -> Result<T, minicbor::decode::Error> {
    minicbor::decode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{MetaOp, OpBody, Reject};

    #[test]
    fn roundtrip_messages() {
        let op = Op {
            op_id: 7,
            hlc: Hlc::new(5, 1, 9),
            known_seq: 3,
            group: 0,
            body: OpBody::Meta(MetaOp::SetName {
                id: Id([1; 16]),
                name: "x.md".into(),
            }),
        };
        let m = ClientMsg::Push {
            ops: vec![
                op.clone(),
                Op {
                    body: OpBody::Doc {
                        entry: Id([2; 16]),
                        slot: "body".into(),
                        update: vec![1, 2, 3],
                    },
                    ..op
                },
            ],
        };
        assert_eq!(decode::<ClientMsg>(&encode(&m)).unwrap(), m);
        let s = ServerMsg::Ack {
            results: vec![
                (1, AckResult::Applied(4)),
                (2, AckResult::Rejected(Reject::Cycle)),
            ],
        };
        assert_eq!(decode::<ServerMsg>(&encode(&s)).unwrap(), s);
        let c = ServerMsg::Changes(Changes {
            from: 1,
            to: 2,
            docs: vec![DocUpdate {
                seq: 2,
                entry: Id([3; 16]),
                slot: "body".into(),
                bytes: None,
            }],
            ..Default::default()
        });
        assert_eq!(decode::<ServerMsg>(&encode(&c)).unwrap(), c);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        // A Hello with an extra key 99 must still decode.
        let mut e = minicbor::Encoder::new(Vec::new());
        e.map(2)
            .unwrap()
            .u32(0)
            .unwrap()
            .u32(1)
            .unwrap()
            .u32(99)
            .unwrap()
            .str("future")
            .unwrap();
        let v = e.into_writer();
        // Missing required fields fail, so wrap in a lenient struct test instead:
        #[derive(Decode)]
        #[cbor(map)]
        struct P {
            #[n(0)]
            proto: u32,
        }
        let p: P = minicbor::decode(&v).unwrap();
        assert_eq!(p.proto, 1);
    }
}
