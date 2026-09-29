//! JSON shapes shared by the UI hosts (the WASM worker and the native Tauri backend): entries
//! as the UI sees them, meta intents from the UI, sync status, quarantine, projection items.

use crate::client::{Client, Status};
use crate::ids::{Hash, Id};
use crate::model::{BlobInfo, Entry, PropValue};
use crate::ops::MetaOp;
use serde::Deserialize;
use serde_json::{json, Value};

fn id_of(s: &str) -> Result<Id, String> {
    Id::parse(s).ok_or_else(|| "bad id".to_string())
}

fn hash_of(s: &str) -> Result<Hash, String> {
    Hash::parse_hex(s).ok_or_else(|| "bad hash".to_string())
}

pub fn prop_json(v: &PropValue) -> Value {
    if let Some(s) = v.as_str() {
        return Value::String(s);
    }
    if let Some(b) = v.as_bool() {
        return Value::Bool(b);
    }
    if let Some(i) = v.as_int() {
        return json!(i);
    }
    Value::Null
}

/// JSON view of an entry for the UI.
pub fn entry_json(e: &Entry) -> Value {
    let mut props = serde_json::Map::new();
    for (k, v) in &e.props {
        props.insert(k.clone(), prop_json(v));
    }
    json!({
        "id": e.id.to_string(),
        "kind": e.kind,
        "parent": e.parent.map(|p| p.to_string()),
        "name": e.name,
        "trashed": e.trashed.map(|t| json!({"batch": t.batch.to_string(), "at": t.at})),
        "visible": e.tree_visible,
        "blob": e.blob.map(|h| h.to_hex()),
        "created": e.created_at,
        "modified": e.modified_at,
        "purged": e.purged,
        "seq": e.seq,
        "props": props,
    })
}

/// An entry plus what is known about its blob (size, mime, oriented dimensions).
pub fn entry_view(e: &Entry, facts: Option<&BlobInfo>) -> Value {
    let mut v = entry_json(e);
    if let Some(f) = facts {
        v["blobInfo"] = json!({ "size": f.size, "mime": f.mime, "width": f.width, "height": f.height, "orientation": f.orientation });
    }
    v
}

/// Entries by id as the UI sees them (`{id, deleted: true}` for ones that are gone).
pub fn entries_view(c: &Client, ids: &[Id]) -> Vec<Value> {
    ids.iter()
        .map(|id| match c.view().get(id) {
            Some(e) => entry_view(e, e.blob.and_then(|h| c.blob_facts(&h))),
            None => json!({"id": id.to_string(), "deleted": true}),
        })
        .collect()
}

/// The whole optimistic view.
pub fn view(c: &Client) -> Vec<Value> {
    c.view()
        .iter()
        .map(|e| entry_view(e, e.blob.and_then(|h| c.blob_facts(&h))))
        .collect()
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum OpJson {
    #[serde(rename_all = "camelCase")]
    Create {
        id: String,
        kind: String,
        parent: Option<String>,
        name: String,
        #[serde(default = "yes")]
        visible: bool,
        blob: Option<String>,
        blob_info: Option<BlobInfoJson>,
        created: Option<u64>,
        modified: Option<u64>,
    },
    SetParent {
        id: String,
        parent: Option<String>,
    },
    SetName {
        id: String,
        name: String,
    },
    SetVisible {
        id: String,
        visible: bool,
    },
    #[serde(rename_all = "camelCase")]
    SetBlob {
        id: String,
        blob: String,
        blob_info: Option<BlobInfoJson>,
    },
    Trash {
        id: String,
    },
    Restore {
        target: String,
    },
    Purge {
        id: String,
    },
    SetProp {
        id: String,
        key: String,
        value: Option<Value>,
    },
    SetTimes {
        id: String,
        created: Option<u64>,
        modified: Option<u64>,
    },
}

fn yes() -> bool {
    true
}

#[derive(Deserialize, Default)]
pub struct BlobInfoJson {
    size: u64,
    mime: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    orientation: Option<u8>,
}

impl From<BlobInfoJson> for BlobInfo {
    fn from(b: BlobInfoJson) -> BlobInfo {
        BlobInfo {
            size: b.size,
            mime: b.mime,
            width: b.width,
            height: b.height,
            orientation: b.orientation,
        }
    }
}

pub fn parse_op(o: OpJson) -> Result<MetaOp, String> {
    let opt =
        |p: Option<String>| -> Result<Option<Id>, String> { p.map(|s| id_of(&s)).transpose() };
    Ok(match o {
        OpJson::Create {
            id,
            kind,
            parent,
            name,
            visible,
            blob,
            blob_info,
            created,
            modified,
        } => MetaOp::Create {
            id: id_of(&id)?,
            kind,
            parent: opt(parent)?,
            name,
            tree_visible: visible,
            blob: blob.map(|h| hash_of(&h)).transpose()?,
            blob_info: blob_info.map(Into::into),
            created_at: created,
            modified_at: modified,
            props: vec![],
        },
        OpJson::SetParent { id, parent } => MetaOp::SetParent {
            id: id_of(&id)?,
            parent: opt(parent)?,
        },
        OpJson::SetName { id, name } => MetaOp::SetName {
            id: id_of(&id)?,
            name,
        },
        OpJson::SetVisible { id, visible } => MetaOp::SetVisible {
            id: id_of(&id)?,
            visible,
        },
        OpJson::SetBlob {
            id,
            blob,
            blob_info,
        } => MetaOp::SetBlob {
            id: id_of(&id)?,
            blob: hash_of(&blob)?,
            blob_info: blob_info.map(Into::into),
        },
        OpJson::Trash { id } => MetaOp::Trash { id: id_of(&id)? },
        OpJson::Restore { target } => MetaOp::Restore {
            target: id_of(&target)?,
        },
        OpJson::Purge { id } => MetaOp::Purge { id: id_of(&id)? },
        OpJson::SetProp { id, key, value } => MetaOp::SetProp {
            id: id_of(&id)?,
            key,
            value: match value {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(PropValue::str(&s)),
                Some(Value::Bool(b)) => Some(PropValue::bool(b)),
                Some(Value::Number(n)) => Some(PropValue::int(n.as_i64().unwrap_or(0))),
                Some(_) => return Err("unsupported prop value".into()),
            },
        },
        OpJson::SetTimes {
            id,
            created,
            modified,
        } => MetaOp::SetTimes {
            id: id_of(&id)?,
            created,
            modified,
        },
    })
}

/// Parses a JSON array of meta intents.
pub fn parse_ops(ops_json: &str) -> Result<Vec<MetaOp>, String> {
    let ops: Vec<OpJson> = serde_json::from_str(ops_json).map_err(|e| e.to_string())?;
    ops.into_iter().map(parse_op).collect()
}

/// Sync status for the status bar.
pub fn status(c: &Client) -> Value {
    let mut s = match c.status() {
        Status::Synced => json!({"state": "synced"}),
        Status::Syncing { pending } => json!({"state": "syncing", "pending": pending}),
        Status::Offline { pending } => json!({"state": "offline", "pending": pending}),
        Status::Error(e) => json!({"state": "error", "error": e}),
    };
    let p = c.blobs.progress();
    s["uploads"] = json!({"pending": p.uploads_pending, "total": p.uploads_total});
    s["downloads"] = json!(p.downloads_pending);
    s["quarantined"] = json!(c.quarantine().count());
    s
}

/// Rejected ops kept in quarantine.
pub fn quarantine(c: &Client) -> Vec<Value> {
    c.quarantine()
        .map(|q| json!({"opId": q.op.op_id, "reason": format!("{:?}", q.reason), "op": format!("{:?}", q.op.body)}))
        .collect()
}

/// Blob facts as JSON.
pub fn blob_info(b: &BlobInfo) -> Value {
    json!({"size": b.size, "mime": b.mime, "width": b.width, "height": b.height, "orientation": b.orientation})
}
