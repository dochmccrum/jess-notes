//! jess-core: the single source of truth for Jess Notes semantics (DESIGN D5).

pub mod apply;
pub mod hlc;
pub mod ids;
pub mod model;
pub mod names;
pub mod ops;
pub mod proto;
pub mod state;

pub use ids::{Hash, Id};
pub mod blobs;
pub mod client;
#[cfg(feature = "yrs")]
pub mod doc;
pub mod kv;
pub mod links;
pub mod resolve;
pub mod rewrite;
