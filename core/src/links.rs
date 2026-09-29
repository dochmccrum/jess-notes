//! Link / tag / frontmatter extraction (DESIGN §9.1). A single pass over the note text that skips
//! fenced and indented code, inline code, `%%comments%%` and maths.
//! All ranges are UTF-16 code-unit offsets (matching Yjs / yrs `OffsetKind::Utf16`).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Syntax {
    Wiki,
    Markdown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub syntax: Syntax,
    pub embed: bool,
    /// Link target as it resolves: wiki target text, or the percent-decoded markdown URL path.
    pub target: String,
    /// Target exactly as written (markdown: still percent-encoded, without `<>`).
    pub raw_target: String,
    /// `#Heading`, `#^block`, `#page=3&height=600` (with the leading `#`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subpath: Option<String>,
    /// Wiki alias / size spec, or markdown link text / alt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    /// Markdown destination written as `<...>`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub angle: bool,
    /// Whole link, UTF-16 `[start, end)`.
    pub range: (u32, u32),
    /// The target substring (what a rename rewrite replaces), UTF-16 `[start, end)`.
    pub target_range: (u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    /// UTF-16 range of `#tag` in the body; `None` for frontmatter tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<(u32, u32)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Extracted {
    pub links: Vec<Link>,
    pub tags: Vec<Tag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frontmatter: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Byte range of the frontmatter block (including delimiters), if any.
    #[serde(skip)]
    pub frontmatter_bytes: Option<(usize, usize)>,
}

/// Is `s` an external URL (`scheme:`)?
pub fn is_external(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || !b[0].is_ascii_alphabetic() {
        return false;
    }
    for (i, &c) in b.iter().enumerate().skip(1) {
        if c == b':' {
            // Treat single letters (`C:`) as paths, not schemes.
            return i > 1;
        }
        if !(c.is_ascii_alphanumeric() || c == b'+' || c == b'.' || c == b'-') {
            return false;
        }
    }
    false
}

struct Scanner<'a> {
    t: &'a str,
    b: &'a [u8],
    links: Vec<(Link, [usize; 4])>,
    tags: Vec<(String, usize, usize)>,
    pending_skip: Option<usize>,
}

fn is_blank(line: &[u8]) -> bool {
    line.iter().all(|c| matches!(c, b' ' | b'\t' | b'\r'))
}

fn line_end(b: &[u8], from: usize) -> usize {
    memchr(b'\n', &b[from..])
        .map(|i| from + i)
        .unwrap_or(b.len())
}

fn memchr(c: u8, s: &[u8]) -> Option<usize> {
    s.iter().position(|&x| x == c)
}

/// Fence opener: up to 3 spaces, then ``` or ~~~ (≥3). Returns (char, len).
fn fence_open(line: &[u8]) -> Option<(u8, usize)> {
    let ind = line.iter().take_while(|&&c| c == b' ').count();
    if ind > 3 {
        return None;
    }
    let rest = &line[ind..];
    let c = *rest.first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = rest.iter().take_while(|&&x| x == c).count();
    if n < 3 {
        return None;
    }
    if c == b'`' && rest[n..].contains(&b'`') {
        return None;
    }
    Some((c, n))
}

fn fence_close(line: &[u8], ch: u8, len: usize) -> bool {
    let ind = line.iter().take_while(|&&c| c == b' ').count();
    if ind > 3 {
        return false;
    }
    let rest = &line[ind..];
    let n = rest.iter().take_while(|&&x| x == ch).count();
    n >= len && is_blank(&rest[n..])
}

impl<'a> Scanner<'a> {
    /// Finds `pat` starting at `from`, not crossing a blank line. Returns the match start.
    fn find_in_para(&self, from: usize, pat: &[u8]) -> Option<usize> {
        let b = self.b;
        let mut i = from;
        while i + pat.len() <= b.len() {
            if b[i] == b'\n' {
                // blank line ahead?
                let e = line_end(b, i + 1);
                if is_blank(&b[i + 1..e]) {
                    return None;
                }
            }
            if &b[i..i + pat.len()] == pat {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn scan_blocks(&mut self, start: usize) {
        let b = self.b;
        let mut pos = start;
        let mut prev_blank = true;
        let mut in_para = false;
        while pos < b.len() {
            let eol = line_end(b, pos);
            let line = &b[pos..eol];
            let next = (eol + 1).min(b.len());
            if let Some((ch, n)) = fence_open(line) {
                // Skip to the closing fence (or end of document).
                let mut p = next;
                let mut close = b.len();
                while p < b.len() {
                    let e = line_end(b, p);
                    if fence_close(&b[p..e], ch, n) {
                        close = (e + 1).min(b.len());
                        break;
                    }
                    p = e + 1;
                }
                pos = close;
                prev_blank = true;
                in_para = false;
                continue;
            }
            if is_blank(line) {
                prev_blank = true;
                in_para = false;
                pos = next;
                continue;
            }
            let indented = line.starts_with(b"    ") || line.starts_with(b"\t");
            if indented && prev_blank && !in_para {
                // Indented code block: consume following indented or blank lines.
                let mut p = next;
                let mut last = next;
                while p < b.len() {
                    let e = line_end(b, p);
                    let l = &b[p..e];
                    if l.starts_with(b"    ") || l.starts_with(b"\t") {
                        last = (e + 1).min(b.len());
                    } else if !is_blank(l) {
                        break;
                    }
                    p = e + 1;
                }
                pos = last;
                prev_blank = true;
                continue;
            }
            let trimmed_start = line
                .iter()
                .take_while(|&&c| c == b' ' || c == b'\t')
                .count();
            if line[trimmed_start..].starts_with(b"$$") {
                // Display maths: single-line $$…$$ or a block through the next line containing $$.
                let after = pos + trimmed_start + 2;
                if let Some(rel) = find_bytes(&b[after..eol], b"$$") {
                    let _ = rel;
                    pos = next;
                    prev_blank = false;
                    in_para = true;
                    continue;
                }
                let mut p = next;
                let mut close = None;
                while p < b.len() {
                    let e = line_end(b, p);
                    if find_bytes(&b[p..e], b"$$").is_some() {
                        close = Some((e + 1).min(b.len()));
                        break;
                    }
                    p = e + 1;
                }
                if let Some(c) = close {
                    pos = c;
                    prev_blank = false;
                    in_para = false;
                    continue;
                }
                // Unterminated: plain text.
            }
            // A paragraph run: scan inline until a blank line or a line that starts a block.
            let end = self.para_end(pos);
            pos = self.scan_para(pos, end);
            prev_blank = false;
            in_para = false;
        }
    }

    /// End of the paragraph run that starts at `pos` (exclusive, at a line start).
    fn para_end(&self, pos: usize) -> usize {
        let b = self.b;
        let mut p = line_end(b, pos) + 1;
        while p < b.len() {
            let e = line_end(b, p);
            let l = &b[p..e];
            if is_blank(l) || fence_open(l).is_some() {
                return p;
            }
            let ts = l.iter().take_while(|&&c| c == b' ' || c == b'\t').count();
            if l[ts..].starts_with(b"$$") {
                return p;
            }
            p = e + 1;
        }
        b.len()
    }

    fn scan_inline(&mut self, start: usize, end: usize, depth: u32) {
        let b = self.b;
        let mut i = start;
        while i < end {
            let c = b[i];
            match c {
                b'\\' => {
                    i += 2;
                    continue;
                }
                b'`' => {
                    let n = b[i..end].iter().take_while(|&&x| x == b'`').count();
                    let mut j = i + n;
                    let mut found = None;
                    while j < end {
                        if b[j] == b'`' {
                            let m = b[j..end].iter().take_while(|&&x| x == b'`').count();
                            if m == n {
                                found = Some(j + m);
                                break;
                            }
                            j += m;
                        } else {
                            j += 1;
                        }
                    }
                    i = found.unwrap_or(i + n);
                    continue;
                }
                b'%' if b.get(i + 1) == Some(&b'%') => {
                    // Comments may span paragraphs; an unterminated %% is literal.
                    match find_bytes(&b[i + 2..], b"%%") {
                        Some(rel) => {
                            let close = i + 2 + rel + 2;
                            if close > end {
                                // Continue block scanning after the comment.
                                self.pending_skip = Some(close);
                                return;
                            }
                            i = close;
                        }
                        None => i += 2,
                    }
                    continue;
                }
                b'$' => {
                    if b.get(i + 1) == Some(&b'$') {
                        if let Some(j) = self.find_in_para(i + 2, b"$$") {
                            if j <= end {
                                i = j + 2;
                                continue;
                            }
                        }
                        i += 2;
                        continue;
                    }
                    if let Some(close) = self.inline_math_end(i, end) {
                        i = close;
                        continue;
                    }
                    i += 1;
                    continue;
                }
                b'[' | b'!' => {
                    let embed = c == b'!';
                    let o = if embed { i + 1 } else { i };
                    if embed && b.get(o) != Some(&b'[') {
                        i += 1;
                        continue;
                    }
                    if b.get(o + 1) == Some(&b'[') {
                        if let Some(next) = self.wiki(i, o, embed, end) {
                            i = next;
                            continue;
                        }
                        i = o + 2;
                        continue;
                    }
                    if let Some(next) = self.md_link(i, o, embed, end, depth) {
                        i = next;
                        continue;
                    }
                    i = o + 1;
                    continue;
                }
                b'#' => {
                    let prev_ok = i == 0 || matches!(b[i - 1], b' ' | b'\t' | b'\n' | b'\r');
                    if prev_ok {
                        let body_start = i + 1;
                        let body: String = self.t[body_start..end]
                            .chars()
                            .take_while(|ch| {
                                ch.is_alphanumeric() || *ch == '_' || *ch == '/' || *ch == '-'
                            })
                            .collect();
                        if !body.is_empty() && body.chars().any(|ch| !ch.is_ascii_digit()) {
                            let e = body_start + body.len();
                            self.tags.push((body, i, e));
                            i = e;
                            continue;
                        }
                    }
                    i += 1;
                    continue;
                }
                _ => {
                    // Advance by a whole UTF-8 char.
                    i += utf8_len(c);
                }
            }
        }
    }

    /// Obsidian inline maths: opening `$` followed by non-space and not `$`; closing `$`
    /// preceded by non-space and not followed by a digit; no blank line inside.
    fn inline_math_end(&self, i: usize, end: usize) -> Option<usize> {
        let b = self.b;
        let n = *b.get(i + 1)?;
        if n == b' ' || n == b'\t' || n == b'\n' || n == b'\r' || n == b'$' {
            return None;
        }
        let mut j = i + 1;
        while j < end {
            match b[j] {
                b'\\' => {
                    j += 2;
                    continue;
                }
                b'\n' => {
                    let e = line_end(b, j + 1);
                    if is_blank(&b[j + 1..e]) {
                        return None;
                    }
                }
                b'$' => {
                    let prev = b[j - 1];
                    let next = b.get(j + 1).copied().unwrap_or(b' ');
                    if !matches!(prev, b' ' | b'\t' | b'\n' | b'\r')
                        && !next.is_ascii_digit()
                        && j > i + 1
                    {
                        return Some(j + 1);
                    }
                }
                _ => {}
            }
            j += 1;
        }
        None
    }

    fn wiki(&mut self, start: usize, o: usize, embed: bool, end: usize) -> Option<usize> {
        let b = self.b;
        let inner_start = o + 2;
        let eol = line_end(b, inner_start).min(end);
        let close = inner_start + find_bytes(&b[inner_start..eol], b"]]")?;
        let inner = &self.t[inner_start..close];
        if let Some(k) = inner.find("[[") {
            // `[[a [[b]]`: restart at the inner opener.
            return Some(inner_start + k);
        }
        if inner.trim().is_empty() {
            return Some(close + 2);
        }
        let (left, display) = match inner.find('|') {
            Some(p) => (&inner[..p], Some(inner[p + 1..].to_string())),
            None => (inner, None),
        };
        // `\|` inside tables: the backslash belongs to the escape, not the target.
        let left = left.strip_suffix('\\').unwrap_or(left);
        let (tgt, sub) = match left.find('#') {
            Some(h) => (&left[..h], Some(left[h..].to_string())),
            None => (left, None),
        };
        let lead = tgt.len() - tgt.trim_start().len();
        let t_trim = tgt.trim();
        let ts = inner_start + lead;
        let te = ts + t_trim.len();
        let link = Link {
            syntax: Syntax::Wiki,
            embed,
            target: t_trim.to_string(),
            raw_target: t_trim.to_string(),
            subpath: sub,
            display,
            angle: false,
            range: (0, 0),
            target_range: (0, 0),
        };
        self.links.push((link, [start, close + 2, ts, te]));
        Some(close + 2)
    }

    fn md_link(
        &mut self,
        start: usize,
        o: usize,
        embed: bool,
        end: usize,
        depth: u32,
    ) -> Option<usize> {
        let b = self.b;
        // Matching `]` with bracket balance, within the paragraph.
        let mut j = o + 1;
        let mut bal = 1;
        while j < end {
            match b[j] {
                b'\\' => j += 1,
                b'[' => bal += 1,
                b']' => {
                    bal -= 1;
                    if bal == 0 {
                        break;
                    }
                }
                b'\n' => {
                    let e = line_end(b, j + 1);
                    if is_blank(&b[j + 1..e]) {
                        return None;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if j >= end || b.get(j + 1) != Some(&b'(') {
            return None;
        }
        let text_range = (o + 1, j);
        let mut k = j + 2;
        while k < end && (b[k] == b' ' || b[k] == b'\t') {
            k += 1;
        }
        let (dest_s, dest_e, angle, mut after);
        if b.get(k) == Some(&b'<') {
            let s = k + 1;
            let mut e = s;
            while e < end && b[e] != b'>' && b[e] != b'\n' && b[e] != b'<' {
                if b[e] == b'\\' {
                    e += 1;
                }
                e += 1;
            }
            if e >= end || b[e] != b'>' {
                return None;
            }
            dest_s = s;
            dest_e = e;
            angle = true;
            after = e + 1;
        } else {
            let s = k;
            let mut e = s;
            let mut par = 0i32;
            while e < end {
                let c = b[e];
                if c == b'\\' {
                    e += 2;
                    continue;
                }
                if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' || c < 0x20 {
                    break;
                }
                if c == b'(' {
                    par += 1;
                } else if c == b')' {
                    if par == 0 {
                        break;
                    }
                    par -= 1;
                }
                e += 1;
            }
            dest_s = s;
            dest_e = e.min(end);
            angle = false;
            after = dest_e;
        }
        // Optional title, then `)`.
        while after < end && matches!(b[after], b' ' | b'\t' | b'\n' | b'\r') {
            after += 1;
        }
        if after < end && matches!(b[after], b'"' | b'\'' | b'(') {
            let q = if b[after] == b'(' { b')' } else { b[after] };
            let mut t = after + 1;
            while t < end && b[t] != q {
                if b[t] == b'\\' {
                    t += 1;
                }
                t += 1;
            }
            after = t + 1;
            while after < end && matches!(b[after], b' ' | b'\t' | b'\n' | b'\r') {
                after += 1;
            }
        }
        if after >= end || b[after] != b')' {
            return None;
        }
        let link_end = after + 1;
        // Nested content (e.g. an image inside link text).
        if depth < 4 {
            self.scan_inline(text_range.0, text_range.1, depth + 1);
        }
        let dest = &self.t[dest_s..dest_e];
        let (path_part, sub) = match dest.find('#') {
            Some(h) => (&dest[..h], Some(dest[h..].to_string())),
            None => (dest, None),
        };
        if path_part.is_empty() || is_external(path_part) {
            return Some(link_end);
        }
        let decoded = percent_encoding::percent_decode_str(path_part)
            .decode_utf8()
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| path_part.to_string());
        let sub = sub.map(|s| {
            percent_encoding::percent_decode_str(&s)
                .decode_utf8()
                .map(|c| c.into_owned())
                .unwrap_or(s)
        });
        let link = Link {
            syntax: Syntax::Markdown,
            embed,
            target: decoded,
            raw_target: path_part.to_string(),
            subpath: sub,
            display: Some(self.t[text_range.0..text_range.1].to_string()),
            angle,
            range: (0, 0),
            target_range: (0, 0),
        };
        self.links
            .push((link, [start, link_end, dest_s, dest_s + path_part.len()]));
        Some(link_end)
    }
}

impl<'a> Scanner<'a> {
    fn run(&mut self, start: usize) {
        self.scan_blocks(start);
    }

    /// End of the paragraph containing byte `p`.
    fn para_end_from(&self, p: usize) -> usize {
        let b = self.b;
        let mut q = line_end(b, p) + 1;
        while q < b.len() {
            let e = line_end(b, q);
            if is_blank(&b[q..e]) || fence_open(&b[q..e]).is_some() {
                return q;
            }
            q = e + 1;
        }
        b.len()
    }

    /// Inline-scans `[pos, end)`, following `%%` comments that close in later paragraphs.
    fn scan_para(&mut self, pos: usize, end: usize) -> usize {
        let (mut pos, mut end) = (pos, end);
        loop {
            self.pending_skip = None;
            self.scan_inline(pos, end, 0);
            match self.pending_skip.take() {
                Some(p) if p < self.b.len() => {
                    pos = p;
                    end = self.para_end_from(p);
                }
                Some(p) => return p,
                None => return end,
            }
        }
    }
}

fn find_bytes(h: &[u8], n: &[u8]) -> Option<usize> {
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    h.windows(n.len()).position(|w| w == n)
}

fn utf8_len(c: u8) -> usize {
    if c < 0x80 {
        1
    } else if c >> 5 == 0b110 {
        2
    } else if c >> 4 == 0b1110 {
        3
    } else if c >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// Converts sorted byte offsets to UTF-16 offsets in one pass.
fn to_utf16(text: &str, offsets: &mut [(usize, usize)]) -> Vec<u32> {
    // offsets: (byte_offset, index_into_result)
    offsets.sort_unstable();
    let mut out = vec![0u32; offsets.len()];
    let mut u16pos = 0u32;
    let mut it = offsets.iter().peekable();
    for (bi, ch) in text.char_indices() {
        while let Some(&&(off, idx)) = it.peek() {
            if off <= bi {
                out[idx] = u16pos;
                it.next();
            } else {
                break;
            }
        }
        u16pos += ch.len_utf16() as u32;
    }
    for &(_, idx) in it {
        out[idx] = u16pos;
    }
    out
}

/// Detects a frontmatter block at the start (optionally after a BOM). Returns
/// (block byte range incl. delimiters, inner yaml byte range).
pub fn frontmatter_range(text: &str) -> Option<((usize, usize), (usize, usize))> {
    let start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let b = text.as_bytes();
    let first_end = line_end(b, start);
    let first = text[start..first_end].trim_end_matches('\r');
    if first != "---" {
        return None;
    }
    let inner_start = (first_end + 1).min(b.len());
    let mut p = inner_start;
    while p < b.len() {
        let e = line_end(b, p);
        let l = text[p..e].trim_end_matches(['\r', ' ', '\t']);
        if l == "---" || l == "..." {
            return Some(((start, (e + 1).min(b.len())), (inner_start, p)));
        }
        p = e + 1;
    }
    None
}

fn yaml_to_json(y: &saphyr::Yaml) -> serde_json::Value {
    use saphyr::{Scalar, Yaml};
    use serde_json::Value as J;
    match y {
        Yaml::Value(s) => match s {
            Scalar::Null => J::Null,
            Scalar::Boolean(b) => J::Bool(*b),
            Scalar::Integer(i) => J::from(*i),
            Scalar::FloatingPoint(f) => serde_json::Number::from_f64(f.into_inner())
                .map(J::Number)
                .unwrap_or(J::Null),
            Scalar::String(s) => J::String(s.to_string()),
        },
        Yaml::Representation(s, _, _) => J::String(s.to_string()),
        Yaml::Sequence(v) => J::Array(v.iter().map(yaml_to_json).collect()),
        Yaml::Mapping(m) => {
            let mut o = serde_json::Map::new();
            for (k, v) in m.iter() {
                let key = match yaml_to_json(k) {
                    J::String(s) => s,
                    other => other.to_string(),
                };
                o.insert(key, yaml_to_json(v));
            }
            J::Object(o)
        }
        Yaml::Tagged(_, inner) => yaml_to_json(inner),
        _ => J::Null,
    }
}

fn string_list(v: &serde_json::Value) -> Vec<String> {
    use serde_json::Value as J;
    match v {
        J::String(s) => s
            .split([',', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        J::Array(a) => a
            .iter()
            .filter_map(|x| match x {
                J::String(s) => Some(s.trim().to_string()),
                J::Number(n) => Some(n.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect(),
        J::Number(n) => vec![n.to_string()],
        _ => vec![],
    }
}

/// Extracts links, tags and frontmatter from note text.
pub fn extract(text: &str) -> Extracted {
    let mut out = Extracted::default();
    let mut body_start = if text.starts_with('\u{feff}') { 3 } else { 0 };
    let mut sc = Scanner {
        t: text,
        b: text.as_bytes(),
        links: Vec::new(),
        tags: Vec::new(),
        pending_skip: None,
    };
    if let Some(((fs, fe), (is, ie))) = frontmatter_range(text) {
        out.frontmatter_bytes = Some((fs, fe));
        body_start = fe;
        let yaml = &text[is..ie];
        use saphyr::LoadableYamlNode;
        if let Ok(docs) = saphyr::Yaml::load_from_str(yaml) {
            if let Some(d) = docs.first() {
                let j = yaml_to_json(d);
                if let serde_json::Value::Object(o) = &j {
                    for key in ["tags", "tag"] {
                        if let Some(v) = o.get(key) {
                            for t in string_list(v) {
                                out.tags.push(Tag {
                                    name: t.trim_start_matches('#').to_string(),
                                    range: None,
                                });
                            }
                        }
                    }
                    for key in ["aliases", "alias"] {
                        if let Some(v) = o.get(key) {
                            out.aliases.extend(string_list_aliases(v));
                        }
                    }
                }
                out.frontmatter = Some(j);
            }
        }
        // Wikilinks inside frontmatter values are links too (Obsidian properties).
        let mut p = is;
        while p < ie {
            let e = line_end(sc.b, p).min(ie);
            let mut i = p;
            while i + 1 < e {
                if sc.b[i] == b'[' && sc.b[i + 1] == b'[' {
                    let embed = i > 0 && sc.b[i - 1] == b'!';
                    let s = if embed { i - 1 } else { i };
                    match sc.wiki(s, i, embed, e) {
                        Some(n) => i = n,
                        None => i += 2,
                    }
                } else {
                    i += 1;
                }
            }
            p = e + 1;
        }
    }
    sc.run(body_start);
    // Convert byte ranges to UTF-16.
    let mut offs = Vec::new();
    for (i, (_, r)) in sc.links.iter().enumerate() {
        for (k, &o) in r.iter().enumerate() {
            offs.push((o, i * 4 + k));
        }
    }
    let base = sc.links.len() * 4;
    for (i, (_, s, e)) in sc.tags.iter().enumerate() {
        offs.push((*s, base + i * 2));
        offs.push((*e, base + i * 2 + 1));
    }
    let u = to_utf16(text, &mut offs);
    let mut links: Vec<Link> = sc
        .links
        .into_iter()
        .enumerate()
        .map(|(i, (mut l, _))| {
            l.range = (u[i * 4], u[i * 4 + 1]);
            l.target_range = (u[i * 4 + 2], u[i * 4 + 3]);
            l
        })
        .collect();
    links.sort_by_key(|l| l.range);
    out.links = links;
    for (i, (name, _, _)) in sc.tags.into_iter().enumerate() {
        out.tags.push(Tag {
            name,
            range: Some((u[base + i * 2], u[base + i * 2 + 1])),
        });
    }
    out
}

fn string_list_aliases(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => vec![s.trim().to_string()],
        other => string_list(other),
    }
}

/// Size spec of an embed display (`300` or `300x200`).
pub fn parse_size(display: &str) -> Option<(u32, Option<u32>)> {
    let d = display.trim();
    let (w, h) = match d.split_once('x') {
        Some((w, h)) => (w, Some(h)),
        None => (d, None),
    };
    let w: u32 = w.trim().parse().ok()?;
    let h = match h {
        Some(h) => Some(h.trim().parse().ok()?),
        None => None,
    };
    Some((w, h))
}

/// Parses an embed's display into (alt, size): `alt|300` (markdown) or `300x200` (wiki).
pub fn split_alt_size(display: &str) -> (Option<String>, Option<(u32, Option<u32>)>) {
    if let Some(s) = parse_size(display) {
        return (None, Some(s));
    }
    if let Some((alt, size)) = display.rsplit_once('|') {
        if let Some(s) = parse_size(size) {
            return (Some(alt.to_string()), Some(s));
        }
    }
    (Some(display.to_string()), None)
}

/// PDF subpath options: `#page=3&height=600`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PdfSubpath {
    pub page: Option<u32>,
    pub height: Option<u32>,
}

pub fn parse_pdf_subpath(sub: &str) -> PdfSubpath {
    let mut out = PdfSubpath::default();
    for kv in sub.trim_start_matches('#').split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            match k.trim() {
                "page" => out.page = v.trim().parse().ok(),
                "height" => out.height = v.trim().parse().ok(),
                _ => {}
            }
        }
    }
    out
}

/// The lookup key a link is indexed under (§6.6 candidates): lower-cased NFC basename with `.md` removed.
pub fn target_key(target: &str) -> String {
    let target = target.trim();
    let base = target.rsplit('/').next().unwrap_or(target).trim();
    let k = crate::names::lookup_key(base);
    match k.strip_suffix(".md") {
        Some(s) => s.to_string(),
        None => k,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(t: &str) -> Vec<String> {
        extract(t).links.into_iter().map(|l| l.target).collect()
    }

    #[test]
    fn basics() {
        let t = "See [[Note]] and ![[img.png|300x200]] and [x](a%20b.md#H) `[[code]]`";
        let e = extract(t);
        assert_eq!(targets(t), vec!["Note", "img.png", "a b.md"]);
        assert!(e.links[1].embed);
        assert_eq!(e.links[1].display.as_deref(), Some("300x200"));
        assert_eq!(e.links[2].subpath.as_deref(), Some("#H"));
        let l = &e.links[0];
        let u: Vec<u16> = t.encode_utf16().collect();
        assert_eq!(
            String::from_utf16(&u[l.target_range.0 as usize..l.target_range.1 as usize]).unwrap(),
            "Note"
        );
    }

    #[test]
    fn code_and_comments_and_math_skipped() {
        let t =
            "```\n[[a]]\n```\n    [[b]]\n\n%%[[c]]\n\n[[d]]%% [[e]] $[[f]]$ $$\n[[g]]\n$$\n[[h]]";
        assert_eq!(targets(t), vec!["e", "h"]);
    }

    #[test]
    fn tags() {
        let e = extract("#tag and x#no and #123 and #a/b-c_d and # heading\n#émoji");
        let names: Vec<_> = e.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["tag", "a/b-c_d", "émoji"]);
    }

    #[test]
    fn frontmatter() {
        let t = "---\ntags: [a, '#b']\naliases: Foo\nrel: \"[[Other]]\"\n---\nbody #c";
        let e = extract(t);
        let names: Vec<_> = e.tags.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert_eq!(e.aliases, vec!["Foo"]);
        assert_eq!(targets(t), vec!["Other"]);
    }

    #[test]
    fn utf16_offsets_with_emoji() {
        let t = "😀 [[Ünï]]";
        let e = extract(t);
        assert_eq!(e.links[0].range, (3, 10));
        assert_eq!(e.links[0].target_range, (5, 8));
    }
}
