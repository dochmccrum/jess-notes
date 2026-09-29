//! Data model (DESIGN §3): entries, per-field clocks, blob rows.

use crate::hlc::Hlc;
use crate::ids::{Hash, Id};
use minicbor::{Decode, Encode};
use std::collections::BTreeMap;

pub const KIND_FOLDER: &str = "folder";
pub const KIND_MARKDOWN: &str = "markdown";
pub const KIND_PDF: &str = "pdf";
pub const KIND_MEDIA: &str = "media";
pub const KIND_VAULT: &str = "vault";
pub const SLOT_BODY: &str = "body";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Encode, Decode)]
#[cbor(array)]
pub struct Trashed {
    #[n(0)]
    pub batch: Id,
    #[n(1)]
    pub at: u64,
}

/// Per-register HLCs of the winning writes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Clocks {
    #[n(0)]
    pub parent: Hlc,
    #[n(1)]
    pub name: Hlc,
    #[n(2)]
    pub trashed: Hlc,
    #[n(3)]
    pub visible: Hlc,
    #[n(4)]
    pub blob: Hlc,
    #[n(5)]
    pub created: Hlc,
    #[n(6)]
    pub modified: Hlc,
    #[n(7)]
    pub props: BTreeMap<String, Hlc>,
}

/// Content facts about a blob, supplied by the ingesting client and corrected by the server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BlobInfo {
    #[n(0)]
    pub size: u64,
    #[n(1)]
    pub mime: Option<String>,
    #[n(2)]
    pub width: Option<u32>,
    #[n(3)]
    pub height: Option<u32>,
    #[n(4)]
    pub orientation: Option<u8>,
}

/// One vault entry (a row of `entries`).
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct Entry {
    #[n(0)]
    pub id: Id,
    #[n(1)]
    pub kind: String,
    #[n(2)]
    pub parent: Option<Id>,
    #[n(3)]
    pub name: String,
    #[n(4)]
    pub trashed: Option<Trashed>,
    #[n(5)]
    pub tree_visible: bool,
    #[n(6)]
    pub blob: Option<Hash>,
    #[n(7)]
    pub created_at: Option<u64>,
    #[n(8)]
    pub modified_at: Option<u64>,
    /// Per-key LWW properties; values are opaque CBOR items.
    #[n(9)]
    pub props: BTreeMap<String, PropValue>,
    #[n(10)]
    pub purged: bool,
    #[n(11)]
    pub clock: Clocks,
    #[n(12)]
    pub seq: u64,
}

/// An opaque CBOR-encoded property value (`None` deletes the key).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Encode, Decode)]
#[cbor(transparent)]
pub struct PropValue(#[cbor(n(0), with = "minicbor::bytes")] pub Vec<u8>);

impl PropValue {
    pub fn str(s: &str) -> PropValue {
        PropValue(minicbor::to_vec(s).expect("infallible"))
    }
    pub fn int(i: i64) -> PropValue {
        PropValue(minicbor::to_vec(i).expect("infallible"))
    }
    pub fn bool(b: bool) -> PropValue {
        PropValue(minicbor::to_vec(b).expect("infallible"))
    }
    pub fn as_str(&self) -> Option<String> {
        minicbor::decode::<String>(&self.0).ok()
    }
    pub fn as_int(&self) -> Option<i64> {
        minicbor::decode::<i64>(&self.0).ok()
    }
    pub fn as_bool(&self) -> Option<bool> {
        minicbor::decode::<bool>(&self.0).ok()
    }
}

impl Entry {
    pub fn is_folder(&self) -> bool {
        self.kind == KIND_FOLDER
    }
    pub fn is_live(&self) -> bool {
        self.trashed.is_none() && !self.purged
    }
    /// Documents appear in tree/switcher/search (§3.1).
    pub fn is_document(&self) -> bool {
        self.kind == KIND_MARKDOWN || self.kind == KIND_PDF
    }
    /// Participates in link resolution (§9.2): live, non-folder, not the vault record.
    pub fn is_linkable(&self) -> bool {
        self.is_live() && !self.is_folder() && self.kind != KIND_VAULT
    }
}

/// A row of `blobs` (§3.2).
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode)]
#[cbor(map)]
pub struct BlobRow {
    #[n(0)]
    pub hash: Hash,
    #[n(1)]
    pub size: u64,
    #[n(2)]
    pub mime: Option<String>,
    #[n(3)]
    pub width: Option<u32>,
    #[n(4)]
    pub height: Option<u32>,
    #[n(5)]
    pub orientation: Option<u8>,
    #[n(6)]
    pub present: bool,
    #[n(7)]
    pub seq: u64,
}
