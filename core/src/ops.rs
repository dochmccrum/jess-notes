//! The op model (DESIGN §5.1).

use crate::hlc::Hlc;
use crate::ids::{Hash, Id};
use crate::model::{BlobInfo, PropValue};
use minicbor::{Decode, Encode};

/// One replica-authored operation. Identified by `(replica_id, op_id)`.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Op {
    #[n(0)]
    pub op_id: u64,
    #[n(1)]
    pub hlc: Hlc,
    /// Server seq the replica had applied when it created the op.
    #[n(2)]
    pub known_seq: u64,
    /// Ops sharing a non-zero group id (and adjacent in a push) apply atomically.
    #[n(3)]
    pub group: u64,
    #[n(4)]
    pub body: OpBody,
}

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub enum OpBody {
    #[n(0)]
    Meta(#[n(0)] MetaOp),
    /// A Yjs v1 update for doc slot `(entry, slot)`.
    #[n(1)]
    Doc {
        #[n(0)]
        entry: Id,
        #[n(1)]
        slot: String,
        #[n(2)]
        #[cbor(with = "minicbor::bytes")]
        update: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub enum MetaOp {
    #[n(0)]
    Create {
        #[n(0)]
        id: Id,
        #[n(1)]
        kind: String,
        #[n(2)]
        parent: Option<Id>,
        #[n(3)]
        name: String,
        #[n(4)]
        tree_visible: bool,
        #[n(5)]
        blob: Option<Hash>,
        #[n(6)]
        blob_info: Option<BlobInfo>,
        #[n(7)]
        created_at: Option<u64>,
        #[n(8)]
        modified_at: Option<u64>,
        #[n(9)]
        props: Vec<(String, PropValue)>,
    },
    #[n(1)]
    SetParent {
        #[n(0)]
        id: Id,
        #[n(1)]
        parent: Option<Id>,
    },
    #[n(2)]
    SetName {
        #[n(0)]
        id: Id,
        #[n(1)]
        name: String,
    },
    #[n(3)]
    SetVisible {
        #[n(0)]
        id: Id,
        #[n(1)]
        visible: bool,
    },
    #[n(4)]
    SetBlob {
        #[n(0)]
        id: Id,
        #[n(1)]
        blob: Hash,
        #[n(2)]
        blob_info: Option<BlobInfo>,
    },
    #[n(5)]
    Trash {
        #[n(0)]
        id: Id,
    },
    /// Restores a trash batch (batch id) or an entry (entry id).
    #[n(6)]
    Restore {
        #[n(0)]
        target: Id,
    },
    #[n(7)]
    Purge {
        #[n(0)]
        id: Id,
    },
    #[n(8)]
    SetProp {
        #[n(0)]
        id: Id,
        #[n(1)]
        key: String,
        #[n(2)]
        value: Option<PropValue>,
    },
    #[n(9)]
    SetTimes {
        #[n(0)]
        id: Id,
        #[n(1)]
        created: Option<u64>,
        #[n(2)]
        modified: Option<u64>,
    },
}

impl MetaOp {
    pub fn target(&self) -> Id {
        match self {
            MetaOp::Create { id, .. }
            | MetaOp::SetParent { id, .. }
            | MetaOp::SetName { id, .. }
            | MetaOp::SetVisible { id, .. }
            | MetaOp::SetBlob { id, .. }
            | MetaOp::Trash { id }
            | MetaOp::Purge { id }
            | MetaOp::SetProp { id, .. }
            | MetaOp::SetTimes { id, .. } => *id,
            MetaOp::Restore { target } => *target,
        }
    }
}

/// Why the server refused an op. Refused ops go to the client's quarantine, never discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Encode, Decode)]
#[cbor(index_only)]
pub enum Reject {
    #[n(0)]
    Cycle,
    #[n(1)]
    ParentMissing,
    #[n(2)]
    ParentTrashed,
    #[n(3)]
    Purged,
    #[n(4)]
    NotTrashed,
    #[n(5)]
    BadName,
    #[n(6)]
    BadUpdate,
    #[n(7)]
    UnknownEntry,
    #[n(8)]
    GroupFailed,
    #[n(9)]
    Forbidden,
    #[n(10)]
    NotAFolder,
    #[n(11)]
    TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode)]
pub enum AckResult {
    #[n(0)]
    Applied(#[n(0)] u64),
    #[n(1)]
    Duplicate(#[n(0)] u64),
    #[n(2)]
    Rejected(#[n(0)] Reject),
}
