//! Name rules: uniqueness keys (D8), Obsidian name validation, collision suffixes.

use unicode_normalization::UnicodeNormalization;

/// Uniqueness key within a folder: NFC, case-sensitive (D8).
pub fn name_key(name: &str) -> String {
    name.nfc().collect()
}

/// Case-insensitive lookup key used by the link resolver (§9.2).
pub fn lookup_key(s: &str) -> String {
    s.nfc().collect::<String>().to_lowercase()
}

/// Minimal validity enforced by the server for any name (including imported ones).
pub fn is_storable_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

/// Why a name created inside Jess is refused (Obsidian's rules).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Empty,
    ForbiddenChar(char),
    LeadingDot,
    TrailingDotOrSpace,
}

/// Validates a name typed by the user inside Jess (not applied to imports).
pub fn validate_new_name(name: &str) -> Result<(), NameError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(NameError::Empty);
    }
    for c in name.chars() {
        if matches!(
            c,
            '*' | '"' | '\\' | '/' | '<' | '>' | ':' | '|' | '?' | '#' | '^' | '[' | ']'
        ) || c.is_control()
        {
            return Err(NameError::ForbiddenChar(c));
        }
    }
    if name.starts_with('.') {
        return Err(NameError::LeadingDot);
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(NameError::TrailingDotOrSpace);
    }
    Ok(())
}

/// Splits `Name.ext` into (`Name`, `.ext`). Folders and dot-files have no extension.
pub fn split_ext(name: &str, is_folder: bool) -> (&str, &str) {
    if is_folder {
        return (name, "");
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// `Name.md` + n → `Name n.md` (Obsidian's collision convention).
pub fn with_suffix(name: &str, n: u32, is_folder: bool) -> String {
    let (stem, ext) = split_ext(name, is_folder);
    format!("{stem} {n}{ext}")
}

/// Smallest free `Name n.ext` (n ≥ 1) according to `taken`.
pub fn first_free_suffix(
    name: &str,
    is_folder: bool,
    mut taken: impl FnMut(&str) -> bool,
) -> String {
    let mut n = 1;
    loop {
        let cand = with_suffix(name, n, is_folder);
        if !taken(&name_key(&cand)) {
            return cand;
        }
        n += 1;
    }
}

pub const RECOVERED_PREFIX: &str = "Recovered — ";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn suffixes() {
        assert_eq!(with_suffix("Untitled.md", 1, false), "Untitled 1.md");
        assert_eq!(with_suffix("Folder.v2", 2, true), "Folder.v2 2");
        assert_eq!(with_suffix(".hidden", 1, false), ".hidden 1");
        assert_eq!(
            with_suffix("a.excalidraw.md", 1, false),
            "a.excalidraw 1.md"
        );
        let taken = ["Untitled 1.md".to_string()];
        assert_eq!(
            first_free_suffix("Untitled.md", false, |k| taken.iter().any(|t| t == k)),
            "Untitled 2.md"
        );
    }
    #[test]
    fn nfc_keys() {
        assert_eq!(name_key("Cafe\u{301}.md"), name_key("Café.md"));
        assert_ne!(name_key("a.md"), name_key("A.md"));
        assert_eq!(lookup_key("A.MD"), "a.md");
    }
    #[test]
    fn validation() {
        assert!(validate_new_name("Note.md").is_ok());
        assert_eq!(validate_new_name("a|b"), Err(NameError::ForbiddenChar('|')));
        assert_eq!(validate_new_name(" "), Err(NameError::Empty));
    }
}
