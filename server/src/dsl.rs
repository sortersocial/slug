use std::collections::HashMap;

use rand::Rng;

/// Parsed DSL document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub statements: Vec<Stmt>,
}

/// A single statement in the DSL (or prose when using `parse_full`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Hashtag { name: String },
    /// Actor signature / namespace. Example: `@tommy`
    Actor { name: String },
    Item { title: String, body: Option<String> },
    Vote {
        item1: String,
        item2: String,
        ratio_left: i32,
        ratio_right: i32,
        explanation: Option<String>,
    },
    Attribute { name: String },
    Email { address: String },
    Prose { text: String },
}

#[derive(Debug, thiserror::Error)]
pub enum DslError {
    #[error("parse error: {0}")]
    Parse(String),
}

/// Helper to mask balanced blocks to protect them during filtering/parsing.
///
/// Matches the legacy Python parser behavior:
/// - Supports toggle markers (open == close), e.g. ```...```
/// - Supports nested markers (open != close), e.g. { ... { ... } ... }
#[derive(Debug, Default, Clone)]
pub struct BlockMasker {
    pub replacements: HashMap<String, String>,
}

impl BlockMasker {
    pub fn new() -> Self {
        Self {
            replacements: HashMap::new(),
        }
    }

    fn new_token(&mut self) -> String {
        let mut rng = rand::thread_rng();
        let n: u32 = rng.gen();
        let token = format!("__BLOCK_{:08x}__", n);
        // Extremely unlikely collision; if it happens, regenerate.
        if self.replacements.contains_key(&token) {
            return self.new_token();
        }
        token
    }

    /// Replace outermost balanced blocks with tokens.
    pub fn mask(&mut self, text: &str, open_marker: &str, close_marker: &str) -> String {
        if text.is_empty() {
            return text.to_string();
        }

        let text_bytes = text.as_bytes();
        let open_bytes = open_marker.as_bytes();
        let close_bytes = close_marker.as_bytes();

        let is_toggle = open_marker == close_marker;
        let open_len = open_marker.len();
        let close_len = close_marker.len();

        let mut result_parts: Vec<String> = Vec::new();
        let mut current_idx: usize = 0;
        let mut i: usize = 0;
        let mut depth: i32 = 0;
        let mut start_idx: isize = -1;

        while i < text.len() {
            // Check for close marker first (if we are inside a block).
            if depth > 0 && text_bytes[i..].starts_with(close_bytes) {
                if is_toggle {
                    depth = 0; // Toggle off
                } else {
                    depth -= 1;
                }

                i += close_len;

                if depth == 0 {
                    // Found end of outermost block
                    let s = start_idx.max(0) as usize;
                    let original_block = &text[s..i];
                    let token = self.new_token();
                    self.replacements
                        .insert(token.clone(), original_block.to_string());
                    result_parts.push(token);
                    current_idx = i;
                }
                continue;
            }

            // Check for open marker.
            if text_bytes[i..].starts_with(open_bytes) {
                if depth == 0 {
                    // Start of a new outermost block
                    result_parts.push(text[current_idx..i].to_string());
                    start_idx = i as isize;
                }

                if is_toggle {
                    if depth == 0 {
                        depth = 1; // Toggle on
                    }
                } else {
                    depth += 1;
                }

                i += open_len;
                continue;
            }

            // Advance by one byte; this matches the Python implementation which
            // also iterates by index into a string.
            i += 1;
        }

        // Append remaining text.
        result_parts.push(text[current_idx..].to_string());

        // If we have unbalanced markers at the end, the string just stays as is.
        result_parts.concat()
    }

    /// Recursively restore all tokens in the text.
    pub fn unmask(&self, text: &str) -> String {
        if text.is_empty() {
            return text.to_string();
        }

        let mut result = text.to_string();
        loop {
            let mut replaced_count = 0usize;
            for (token, original) in self.replacements.iter() {
                if result.contains(token) {
                    result = result.replace(token, original);
                    replaced_count += 1;
                }
            }
            if replaced_count == 0 {
                break;
            }
        }
        result
    }

    fn extract_body(&self, token: &str) -> String {
        if let Some(original) = self.replacements.get(token) {
            // Important: the stored original may itself contain other __BLOCK_* tokens
            // from earlier masking passes (e.g. code fences inside braces). Always
            // fully unmask before stripping delimiters.
            let original = self.unmask(original);
            if original.starts_with("{{") && original.ends_with("}}") && original.len() >= 4 {
                return original[2..original.len() - 2].trim().to_string();
            }
            if original.starts_with('{') && original.ends_with('}') && original.len() >= 2 {
                return original[1..original.len() - 1].trim().to_string();
            }
            return original.trim().to_string();
        }
        token.to_string()
    }
}

fn mask_all(mut masker: BlockMasker, text: &str) -> (BlockMasker, String) {
    // Mask hierarchy: Code -> Double Brace -> Single Brace.
    let t = masker.mask(text, "```", "```");
    let t = masker.mask(&t, "{{", "}}");
    let t = masker.mask(&t, "{", "}");
    (masker, t)
}

fn is_item_name(s: &str) -> bool {
    // Matches Python: /[a-zA-Z0-9_]+([-][a-zA-Z0-9_]+)*/
    let mut parts = s.split('-');
    let Some(first) = parts.next() else {
        return false;
    };
    if first.is_empty() || !first.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    for p in parts {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    true
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_block_token(s: &str) -> bool {
    // "__BLOCK_" + 8 hex + "__"
    if !s.starts_with("__BLOCK_") || !s.ends_with("__") {
        return false;
    }
    let mid = &s["__BLOCK_".len()..s.len() - 2];
    mid.len() == 8 && mid.chars().all(|c| matches!(c, 'a'..='f' | '0'..='9'))
}

fn looks_like_email(s: &str) -> bool {
    // Lightweight approximation of Python regex:
    // /[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}/
    let (local, domain) = match s.split_once('@') {
        Some(x) => x,
        None => return false,
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    if !local
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._%+-".contains(c))
    {
        return false;
    }
    if !domain
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    {
        return false;
    }
    let parts: Vec<&str> = domain.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    let tld = parts[parts.len() - 1];
    tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
}

fn is_ws_byte(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r')
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    let bytes = s.as_bytes();
    while i < bytes.len() && is_ws_byte(bytes[i]) {
        i += 1;
    }
    i
}

fn parse_item_name_at(s: &str, i: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if i >= bytes.len() {
        return None;
    }

    // ITEM_NAME: [a-zA-Z0-9_]+([-][a-zA-Z0-9_]+)*
    let mut j = i;
    let mut saw = false;
    while j < bytes.len() {
        // After masking, block tokens can be adjacent to the item name
        // (e.g. "/arrived{...}" => "/arrived__BLOCK_deadbeef__").
        // Treat the "__BLOCK_" prefix as a hard boundary so we can parse
        // ITEM_NAME + BLOCK_TOKEN even without whitespace.
        if bytes[j..].starts_with(b"__BLOCK_") {
            break;
        }
        let c = bytes[j] as char;
        if c.is_ascii_alphanumeric() || c == '_' {
            saw = true;
            j += 1;
            continue;
        }
        break;
    }
    if !saw {
        return None;
    }

    while j < bytes.len() && bytes[j] == b'-' {
        let dash = j;
        j += 1;
        let mut seg = false;
        while j < bytes.len() {
            if bytes[j..].starts_with(b"__BLOCK_") {
                break;
            }
            let c = bytes[j] as char;
            if c.is_ascii_alphanumeric() || c == '_' {
                seg = true;
                j += 1;
                continue;
            }
            break;
        }
        if !seg {
            // Trailing '-' or empty segment is not allowed; stop before dash.
            j = dash;
            break;
        }
    }

    let name = &s[i..j];
    Some((name.to_string(), j))
}

fn parse_block_token_at(s: &str, i: usize) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if i >= bytes.len() {
        return None;
    }
    if !bytes[i..].starts_with(b"__BLOCK_") {
        return None;
    }
    // "__BLOCK_" + 8 + "__" = 18 chars total
    if bytes.len().saturating_sub(i) < 18 {
        return None;
    }
    let cand_bytes = &bytes[i..i + 18];
    let cand = std::str::from_utf8(cand_bytes).ok()?;
    if is_block_token(cand) {
        Some((cand.to_string(), i + 18))
    } else {
        None
    }
}

fn parse_comparison_at(s: &str, i: usize) -> Option<((i32, i32), usize)> {
    let bytes = s.as_bytes();
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'>' {
        return Some(((2, 1), i + 1));
    }
    if bytes[i] == b'<' {
        return Some(((1, 2), i + 1));
    }
    if bytes[i] == b'=' {
        return Some(((1, 1), i + 1));
    }

    // NUMBER ":" NUMBER
    let mut j = i;
    if j >= bytes.len() || !(bytes[j] as char).is_ascii_digit() {
        return None;
    }
    while j < bytes.len() && (bytes[j] as char).is_ascii_digit() {
        j += 1;
    }
    if j >= bytes.len() || bytes[j] != b':' {
        return None;
    }
    let left: i32 = s[i..j].parse().ok()?;
    j += 1; // skip ':'
    let k = j;
    if j >= bytes.len() || !(bytes[j] as char).is_ascii_digit() {
        return None;
    }
    while j < bytes.len() && (bytes[j] as char).is_ascii_digit() {
        j += 1;
    }
    let right: i32 = s[k..j].parse().ok()?;
    Some(((left, right), j))
}

fn parse_slash_statement(stripped: &str, masker: &BlockMasker) -> Result<Stmt, DslError> {
    // item: "/" item_ref body?
    // vote: "/" item_ref comparison "/" item_ref body?
    //
    // Important: body token can be adjacent to the item name (no whitespace),
    // e.g. "/arrived{...}" -> "/arrived__BLOCK_x__".
    let s = stripped;
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes[0] != b'/' {
        return Err(DslError::Parse("missing leading '/'".to_string()));
    }

    let mut i = 1;
    let (item1, j) =
        parse_item_name_at(s, i).ok_or_else(|| DslError::Parse("invalid item name".to_string()))?;

    // Either we have:
    // - immediate/whitespace block token => Item
    // - comparison => Vote
    // - whitespace then block token => Item
    // - whitespace then comparison => Vote
    i = skip_ws(s, j);

    // If next is end or a block token => Item.
    if i >= s.len() {
        return Ok(Stmt::Item {
            title: item1,
            body: None,
        });
    }
    if let Some((tok, end)) = parse_block_token_at(s, i) {
        let body = masker.extract_body(&tok);
        let tail = s[end..].trim();
        if !tail.is_empty() {
            return Err(DslError::Parse("extra tokens after item".to_string()));
        }
        return Ok(Stmt::Item {
            title: item1,
            body: Some(body),
        });
    }

    // Otherwise parse comparison then "/item2" then optional body.
    let ((ratio_left, ratio_right), mut k) = parse_comparison_at(s, i)
        .ok_or_else(|| DslError::Parse(format!("invalid comparison near: {}", &s[i..])))?;
    k = skip_ws(s, k);
    if k >= s.len() || s.as_bytes()[k] != b'/' {
        return Err(DslError::Parse("missing '/' for rhs item".to_string()));
    }
    k += 1;
    let (item2, mut m) = parse_item_name_at(s, k)
        .ok_or_else(|| DslError::Parse("invalid rhs item name".to_string()))?;
    m = skip_ws(s, m);

    let mut explanation: Option<String> = None;
    if let Some((tok, end)) = parse_block_token_at(s, m) {
        explanation = Some(masker.extract_body(&tok));
        m = end;
    }
    let tail = s[m..].trim();
    if !tail.is_empty() {
        return Err(DslError::Parse("extra tokens after vote".to_string()));
    }

    Ok(Stmt::Vote {
        item1,
        item2,
        ratio_left,
        ratio_right,
        explanation,
    })
}

fn parse_line(masked_line: &str, masker: &BlockMasker) -> Result<Vec<Stmt>, DslError> {
    let stripped = masked_line.trim_start();
    if stripped.is_empty() {
        return Ok(vec![]);
    }
    let first = stripped.chars().next().unwrap();
    match first {
        '#' => {
            let rest = stripped[1..].trim();
            if !is_item_name(rest) {
                return Err(DslError::Parse(format!("invalid hashtag name: {rest}")));
            }
            Ok(vec![Stmt::Hashtag {
                name: rest.to_string(),
            }])
        }
        ':' => {
            // One or more attributes: ":a :b"
            let mut out = Vec::new();
            for tok in stripped.split_whitespace() {
                let t = tok.trim();
                let Some(name) = t.strip_prefix(':') else {
                    return Err(DslError::Parse(format!("invalid attribute: {t}")));
                };
                if !is_word(name) {
                    return Err(DslError::Parse(format!("invalid attribute: {t}")));
                }
                out.push(Stmt::Attribute {
                    name: name.to_string(),
                });
            }
            if out.is_empty() {
                return Err(DslError::Parse("empty attribute decl".to_string()));
            }
            Ok(out)
        }
        '@' => {
            // Either an actor signature (`@name`) or an email address statement.
            let tok = stripped.trim();
            if looks_like_email(tok) {
                Ok(vec![Stmt::Email {
                    address: tok.to_string(),
                }])
            } else {
                let name = tok.trim_start_matches('@').trim();
                if !is_word(name) {
                    return Err(DslError::Parse(format!("invalid actor: {tok}")));
                }
                Ok(vec![Stmt::Actor {
                    name: name.to_string(),
                }])
            }
        }
        '/' => {
            Ok(vec![parse_slash_statement(stripped, masker)?])
        }
        '!' => {
            // Reserved / future use in Python filter; treat as parse error for now.
            Err(DslError::Parse("unsupported DSL command: !".to_string()))
        }
        _ => Err(DslError::Parse("not a DSL line".to_string())),
    }
}

/// Parse EmailDSL text into AST (expects DSL-only content; prose will error).
pub fn parse(text: &str) -> Result<Document, DslError> {
    let (masker, masked) = mask_all(BlockMasker::new(), text);
    let mut statements: Vec<Stmt> = Vec::new();
    for line in masked.split('\n') {
        let line_trim = line.trim();
        if line_trim.is_empty() {
            continue;
        }
        let line_stmts = parse_line(line, &masker)?;
        statements.extend(line_stmts);
    }
    Ok(Document { statements })
}

/// Parse EmailDSL with stateless line-based filtering (drops non-DSL lines).
pub fn parse_lines(text: &str) -> Result<Document, DslError> {
    let (masker, masked) = mask_all(BlockMasker::new(), text);
    let mut statements: Vec<Stmt> = Vec::new();
    for line in masked.split('\n') {
        let stripped = line.trim_start();
        if stripped.is_empty() {
            continue;
        }
        let first = stripped.chars().next().unwrap();
        if "#:/@!".contains(first) {
            let line_stmts = parse_line(line, &masker)?;
            statements.extend(line_stmts);
        }
    }
    Ok(Document { statements })
}

/// Parse EmailDSL preserving prose for rendering; interleaves `Prose` with DSL nodes.
pub fn parse_full(text: &str) -> Document {
    let (masker, masked) = mask_all(BlockMasker::new(), text);
    let mut statements: Vec<Stmt> = Vec::new();
    let mut prose_buffer: Vec<&str> = Vec::new();

    let flush_prose = |buf: &mut Vec<&str>, out: &mut Vec<Stmt>, masker: &BlockMasker| {
        if buf.is_empty() {
            return;
        }
        let prose_text = buf.join("\n");
        let prose_text = masker.unmask(&prose_text);
        out.push(Stmt::Prose { text: prose_text });
        buf.clear();
    };

    for line in masked.split('\n') {
        let stripped = line.trim_start();
        if !stripped.is_empty() && "#:/@!".contains(stripped.chars().next().unwrap()) {
            // Flush prose buffer first
            flush_prose(&mut prose_buffer, &mut statements, &masker);

            // Parse DSL line; if parsing fails, treat as prose (matches Python behavior).
            match parse_line(line, &masker) {
                Ok(line_stmts) => statements.extend(line_stmts),
                Err(_) => prose_buffer.push(line),
            }
        } else {
            prose_buffer.push(line);
        }
    }

    // Final flush
    flush_prose(&mut prose_buffer, &mut statements, &masker);

    Document { statements }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blockmasker_masks_and_unmasks_nested_braces() {
        let input = "hello {a {b} c} world";
        let mut m = BlockMasker::new();
        let masked = m.mask(input, "{", "}");
        assert!(masked.contains("__BLOCK_"));
        let unmasked = m.unmask(&masked);
        assert_eq!(unmasked, input);
    }

    #[test]
    fn blockmasker_masks_code_blocks_first() {
        let input = "x ```code {not a body}``` y {body}";
        let (masker, masked) = mask_all(BlockMasker::new(), input);
        assert!(masked.contains("__BLOCK_"));
        let roundtrip = masker.unmask(&masked);
        assert_eq!(roundtrip, input);
    }

    #[test]
    fn parse_item_with_body_strips_outer_braces() {
        let input = "/rust { Systems language }";
        let doc = parse(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Item {
                title: "rust".to_string(),
                body: Some("Systems language".to_string()),
            }]
        );
    }

    #[test]
    fn parse_vote_ratio_and_symbols() {
        let d1 = parse("/a 3:1 /b").unwrap();
        assert_eq!(
            d1.statements,
            vec![Stmt::Vote {
                item1: "a".to_string(),
                item2: "b".to_string(),
                ratio_left: 3,
                ratio_right: 1,
                explanation: None
            }]
        );

        let d2 = parse("/a > /b").unwrap();
        assert_eq!(
            d2.statements,
            vec![Stmt::Vote {
                item1: "a".to_string(),
                item2: "b".to_string(),
                ratio_left: 2,
                ratio_right: 1,
                explanation: None
            }]
        );

        let d3 = parse("/a = /b").unwrap();
        assert_eq!(
            d3.statements,
            vec![Stmt::Vote {
                item1: "a".to_string(),
                item2: "b".to_string(),
                ratio_left: 1,
                ratio_right: 1,
                explanation: None
            }]
        );
    }

    #[test]
    fn parse_lines_filters_noise_but_keeps_bodies() {
        let input = r#"
hello there
#tag
/rust {Body line 1
Body line 2}
signature: thanks
"#;
        let doc = parse_lines(input).unwrap();
        assert!(doc.statements.iter().any(|s| matches!(s, Stmt::Hashtag { .. })));
        assert!(doc.statements.iter().any(|s| matches!(s, Stmt::Item { .. })));
    }

    #[test]
    fn parse_full_interleaves_prose() {
        let input = "hello\n#tag\nworld";
        let doc = parse_full(input);
        assert_eq!(
            doc.statements,
            vec![
                Stmt::Prose {
                    text: "hello".to_string()
                },
                Stmt::Hashtag {
                    name: "tag".to_string()
                },
                Stmt::Prose {
                    text: "world".to_string()
                }
            ]
        );
    }

    #[test]
    fn parse_item_body_without_space_like_big_book() {
        let input = "/arrived{I had arrived.}";
        let doc = parse(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Item {
                title: "arrived".to_string(),
                body: Some("I had arrived.".to_string()),
            }]
        );
    }

    #[test]
    fn parse_vote_with_attached_body_without_space() {
        let input = "/a 2:1 /b{because}";
        let doc = parse(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Vote {
                item1: "a".to_string(),
                item2: "b".to_string(),
                ratio_left: 2,
                ratio_right: 1,
                explanation: Some("because".to_string())
            }]
        );
    }
}


