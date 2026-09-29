# Conformance fixtures

Shared by the Rust core (`core/tests/fixtures.rs`) and the TypeScript editor grammar/resolver
(Vitest, phase 3). Ranges are checked by the text they cover, so fixtures stay readable:

- `links.json`: `{ text, links: [{ syntax, embed, target, subpath?, display?, span, target_span }] }`
  where `span` / `target_span` are the exact substrings of the whole link and of its target.
- `tags.json`: `{ text, tags: [name…] }` (body and frontmatter tags, in order: frontmatter first).
- `math.json`: `{ text, math: [{ span, display }] }`.
- `embeds.json`: size specs (`|300`, `|300x200`, `alt|300`) and PDF subpaths (`#page=3&height=600`).
- `resolution.json`: a vault (list of paths) and cases `{ target, syntax, from, expect }`
  (`from` is the linking note's path, `expect` the resolved path or `null`).
