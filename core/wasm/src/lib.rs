//! wasm-bindgen facade over jess-core for the web sync worker (DESIGN D5, §11.2).
//! Phase 1 exposes the pure functions; the sync client bindings arrive with the web app.

use jess_core::links::{extract, Syntax};
use jess_core::resolve::ResolveIndex;
use jess_core::Id;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Links, tags, maths and frontmatter of a note, as JSON.
#[wasm_bindgen(js_name = extract)]
pub fn extract_json(text: &str) -> String {
    serde_json::to_string(&extract(text)).unwrap_or_default()
}

/// A resolve index over vault paths.
#[wasm_bindgen]
pub struct Resolver {
    ix: ResolveIndex,
}

#[wasm_bindgen]
impl Resolver {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Resolver {
        Resolver {
            ix: ResolveIndex::new(),
        }
    }
    /// `id` is the 32-hex-char entry id.
    pub fn insert(&mut self, id: &str, path: &str) {
        if let Some(i) = Id::parse(id) {
            self.ix.insert(i, path);
        }
    }
    pub fn remove(&mut self, id: &str) {
        if let Some(i) = Id::parse(id) {
            self.ix.remove(i);
        }
    }
    /// Returns the resolved entry id, or undefined.
    pub fn resolve(&self, target: &str, markdown: bool, source_folder: &str) -> Option<String> {
        let s = if markdown {
            Syntax::Markdown
        } else {
            Syntax::Wiki
        };
        self.ix
            .resolve(target, s, source_folder)
            .map(|r| r.id.to_string())
    }
}

impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}
