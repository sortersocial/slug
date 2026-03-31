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
    Item { title: String, body: Option<String> },
    Vote {
        item1: String,
        item2: String,
        ratio_left: i32,
        ratio_right: i32,
        /// Required non-empty explanation (from trailing `{ ... }`).
        explanation: String,
    },
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
    let mut name = s.trim();
    if let Some(rest) = name.strip_prefix("~/") {
        name = rest;
    } else if let Some(rest) = name.strip_prefix('/') {
        name = rest;
    }
    if name.is_empty() {
        return false;
    }
    for seg in name.split('/') {
        if seg.is_empty() {
            return false;
        }
        let mut parts = seg.split('-');
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
    }
    true
}

fn is_actor_name(s: &str) -> bool {
    // Actor token parser guardrail (NOT full validation).
    //
    // We purposely keep this permissive so `@...` lines become `Stmt::Actor`
    // and can be validated with helpful error messages at ingest time
    // (see `validate_actor_format` in the API).
    //
    // Rules:
    // - non-empty, <= 256 chars
    // - ASCII + no whitespace
    if s.is_empty() || s.len() > 256 {
        return false;
    }
    s.chars().all(|c| c.is_ascii() && !c.is_whitespace())
}

fn is_block_token(s: &str) -> bool {
    // "__BLOCK_" + 8 hex + "__"
    if !s.starts_with("__BLOCK_") || !s.ends_with("__") {
        return false;
    }
    let mid = &s["__BLOCK_".len()..s.len() - 2];
    mid.len() == 8 && mid.chars().all(|c| matches!(c, 'a'..='f' | '0'..='9'))
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

    // URL_REF: https://... or http://...
    if s[i..].starts_with("https://") || s[i..].starts_with("http://") {
        let mut j = i;
        while j < bytes.len() {
            if bytes[j..].starts_with(b"__BLOCK_") || is_ws_byte(bytes[j]) {
                break;
            }
            j += 1;
        }
        if j <= i {
            return None;
        }
        return Some((s[i..j].to_string(), j));
    }

    // ITEM_REF: "~/" ITEM_NAME  OR  https?:// URL
    // Leading `/path` alone is not valid — use `~/path` for ontology items.
    let mut j = i;
    if bytes[j] != b'~' {
        return None;
    }
    j += 1;
    if j >= bytes.len() || bytes[j] != b'/' {
        return None;
    }
    j += 1;
    let start = j;
    while j < bytes.len() {
        if bytes[j..].starts_with(b"__BLOCK_") {
            break;
        }
        let c = bytes[j] as char;
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/' {
            j += 1;
            continue;
        }
        break;
    }
    if j <= start {
        return None;
    }
    let name = &s[start..j];
    if !is_item_name(name) {
        return None;
    }
    Some((format!("~/{}", name), j))
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

fn slugify_title_to_tag(title: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in title.chars() {
        let lc = c.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() || lc == '_' {
            out.push(lc);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn parse_quoted_thread_statement(stripped: &str, masker: &BlockMasker) -> Result<Vec<Stmt>, DslError> {
    // `"Thread Title" { body }` -> `Hashtag{slug}` + `Prose{body}`
    let bytes = stripped.as_bytes();
    if bytes.is_empty() || bytes[0] != b'"' {
        return Err(DslError::Parse("not a DSL line".to_string()));
    }

    let mut j = 1usize;
    let mut close_quote: Option<usize> = None;
    while j < bytes.len() {
        if bytes[j] == b'\\' {
            j = (j + 2).min(bytes.len());
            continue;
        }
        if bytes[j] == b'"' {
            close_quote = Some(j);
            break;
        }
        j += 1;
    }
    let Some(q_end) = close_quote else {
        return Err(DslError::Parse("unterminated quoted thread title".to_string()));
    };

    let title_raw = &stripped[1..q_end];
    let title = title_raw.replace("\\\"", "\"").replace("\\\\", "\\");
    if title.trim().is_empty() {
        return Err(DslError::Parse("empty quoted thread title".to_string()));
    }

    let mut k = skip_ws(stripped, q_end + 1);
    let Some((tok, end)) = parse_block_token_at(stripped, k) else {
        return Err(DslError::Parse(
            "quoted thread title requires a trailing `{ ... }` body".to_string(),
        ));
    };
    let body = masker.extract_body(&tok);
    if body.trim().is_empty() {
        return Err(DslError::Parse("empty thread body".to_string()));
    }
    k = end;
    let tail = stripped[k..].trim();
    if !tail.is_empty() {
        return Err(DslError::Parse("extra tokens after quoted thread title".to_string()));
    }

    let tag = slugify_title_to_tag(&title);
    if tag.is_empty() {
        return Err(DslError::Parse("thread title slug is empty".to_string()));
    }

    Ok(vec![
        Stmt::Hashtag { name: tag, subtitle: None },
        Stmt::Prose { text: body },
    ])
}

fn parse_item_statement(stripped: &str, masker: &BlockMasker) -> Result<Stmt, DslError> {
    // item: ("~/" | "https://..." | "http://...") item_ref body?
    // vote: same for both operands.
    //
    // Important: body token can be adjacent to the item name (no whitespace),
    // e.g. "~/arrived{...}" -> "~/arrived__BLOCK_x__".
    let s = stripped;
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Err(DslError::Parse("missing item statement".to_string()));
    }

    let (item1, j) =
        parse_item_name_at(s, 0).ok_or_else(|| DslError::Parse("invalid item name".to_string()))?;

    // Either we have:
    // - immediate/whitespace block token => Item
    // - comparison => Vote
    // - whitespace then block token => Item
    // - whitespace then comparison => Vote
    let i = skip_ws(s, j);

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

    // Otherwise parse comparison then "/item2" then REQUIRED body.
    let ((ratio_left, ratio_right), mut k) = parse_comparison_at(s, i)
        .ok_or_else(|| DslError::Parse(format!("invalid comparison near: {}", &s[i..])))?;
    k = skip_ws(s, k);
    let (item2, mut m) = parse_item_name_at(s, k)
        .ok_or_else(|| DslError::Parse("invalid rhs item name".to_string()))?;
    m = skip_ws(s, m);

    let Some((tok, end)) = parse_block_token_at(s, m) else {
        return Err(DslError::Parse(
            "missing vote explanation (add a trailing `{ ... }`)".to_string(),
        ));
    };
    let explanation = masker.extract_body(&tok);
    if explanation.trim().is_empty() {
        return Err(DslError::Parse("empty vote explanation".to_string()));
    }
    m = end;
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
        '#' => Err(DslError::Parse("not a DSL line".to_string())),
        ':' => Err(DslError::Parse(
            "leading ':' is not supported".to_string(),
        )),
        '@' => Err(DslError::Parse("not a DSL line".to_string())),
        '/' => Err(DslError::Parse(
            "item paths must use `~/` (e.g. `~/languages/python`), not a leading `/`"
                .to_string(),
        )),
        '~' => {
            Ok(vec![parse_item_statement(stripped, masker)?])
        }
        'h' => {
            if stripped.starts_with("https://") || stripped.starts_with("http://") {
                Ok(vec![parse_item_statement(stripped, masker)?])
            } else {
                Err(DslError::Parse("not a DSL line".to_string()))
            }
        }
        '!' => {
            // Reserved / future use in Python filter; treat as parse error for now.
            Err(DslError::Parse("unsupported DSL command: !".to_string()))
        }
        '"' => Err(DslError::Parse("not a DSL line".to_string())),
        _ => Err(DslError::Parse("not a DSL line".to_string())),
    }
}

/// Parse EmailDSL preserving prose for rendering; interleaves `Prose` with DSL nodes.
pub fn parse_full(text: &str) -> Result<Document, DslError> {
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
        if !stripped.is_empty()
            && (":/!~".contains(stripped.chars().next().unwrap())
                || (stripped.starts_with("https://") || stripped.starts_with("http://"))
            )
        {
            // Flush prose buffer first
            flush_prose(&mut prose_buffer, &mut statements, &masker);

            // Parse DSL line; DSL statements are not prose, so errors should propagate.
            statements.extend(parse_line(line, &masker)?);
        } else {
            prose_buffer.push(line);
        }
    }

    // Final flush
    flush_prose(&mut prose_buffer, &mut statements, &masker);

    Ok(Document { statements })
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
        let input = "~/rust { Systems language }";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Item {
                title: "~/rust".to_string(),
                body: Some("Systems language".to_string()),
            }]
        );
    }

    #[test]
    fn parse_vote_ratio_and_symbols() {
        let d1 = parse_full("~/a 3:1 ~/b {because}").unwrap();
        assert_eq!(
            d1.statements,
            vec![Stmt::Vote {
                item1: "~/a".to_string(),
                item2: "~/b".to_string(),
                ratio_left: 3,
                ratio_right: 1,
                explanation: "because".to_string()
            }]
        );

        let d2 = parse_full("~/a > ~/b {because}").unwrap();
        assert_eq!(
            d2.statements,
            vec![Stmt::Vote {
                item1: "~/a".to_string(),
                item2: "~/b".to_string(),
                ratio_left: 2,
                ratio_right: 1,
                explanation: "because".to_string()
            }]
        );

        let d3 = parse_full("~/a = ~/b {because}").unwrap();
        assert_eq!(
            d3.statements,
            vec![Stmt::Vote {
                item1: "~/a".to_string(),
                item2: "~/b".to_string(),
                ratio_left: 1,
                ratio_right: 1,
                explanation: "because".to_string()
            }]
        );
    }

    #[test]
    fn parse_full_interleaves_prose() {
        let input = "hello\n#tag\nworld";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![
                Stmt::Prose {
                    text: "hello".to_string()
                },
                Stmt::Prose {
                    text: "#tag\nworld".to_string()
                }
            ]
        );
    }

    #[test]
    fn parse_item_body_without_space_like_big_book() {
        let input = "~/arrived{I had arrived.}";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Item {
                title: "~/arrived".to_string(),
                body: Some("I had arrived.".to_string()),
            }]
        );
    }

    #[test]
    fn parse_vote_with_attached_body_without_space() {
        let input = "~/a 2:1 ~/b{because}";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Vote {
                item1: "~/a".to_string(),
                item2: "~/b".to_string(),
                ratio_left: 2,
                ratio_right: 1,
                explanation: "because".to_string()
            }]
        );
    }

    #[test]
    fn parse_nested_path_item() {
        let input = "~/whitepaper/architectural-choices { Body }";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Item {
                title: "~/whitepaper/architectural-choices".to_string(),
                body: Some("Body".to_string()),
            }]
        );
    }

    #[test]
    fn parse_nested_path_vote() {
        let input = "~/whitepaper/a 3:1 ~/whitepaper/b { because }";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Vote {
                item1: "~/whitepaper/a".to_string(),
                item2: "~/whitepaper/b".to_string(),
                ratio_left: 3,
                ratio_right: 1,
                explanation: "because".to_string()
            }]
        );
    }

    #[test]
    #[test]
    fn parse_rejects_leading_colon() {
        let inputs = [":beauty", ":x", ":"];
        for input in &inputs {
            let result = parse_full(input);
            assert!(result.is_err(), "expected parse error for {input:?}");
            assert!(
                result.unwrap_err().to_string().contains("leading ':' is not supported"),
                "wrong error for {input:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_slash_prefixed_item_path() {
        let result = parse_full("/languages/python { use tilde }");
        assert!(result.is_err(), "leading / item paths must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("~/") && msg.contains("not a leading `/`"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn parse_full_rejects_vote_without_explanation() {
        let input = "~/a {item a}\n~/b {item b}\n~/a 2:1 ~/b\n";
        let result = parse_full(input);
        assert!(result.is_err(), "vote without explanation should fail");
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("missing vote explanation"), "error: {}", err_msg);
    }

    #[test]
    fn parse_full_keeps_quoted_thread_title_statement_as_prose() {
        let input = "\"This is a title\" { This is the body of the post }\n";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Prose {
                text: "\"This is a title\" { This is the body of the post }\n".trim_end_matches('\n').to_string()
            }]
        );
    }

    #[test]
    fn parse_full_keeps_regular_quoted_prose() {
        let input = "She said \"hello\" and left.";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Prose {
                text: "She said \"hello\" and left.".to_string()
            }]
        );
    }

    #[test]
    fn parse_url_item_statement() {
        let input = "https://slug.social/~/music/song-a { body }";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Item {
                title: "https://slug.social/~/music/song-a".to_string(),
                body: Some("body".to_string()),
            }]
        );
    }

    #[test]
    fn parse_url_vote_statement() {
        let input = "https://slug.social/~/music/a 3:1 https://slug.social/~/music/b { because }";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Vote {
                item1: "https://slug.social/~/music/a".to_string(),
                item2: "https://slug.social/~/music/b".to_string(),
                ratio_left: 3,
                ratio_right: 1,
                explanation: "because".to_string(),
            }]
        );
    }
}


