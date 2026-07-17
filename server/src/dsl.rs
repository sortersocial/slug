use std::collections::HashMap;

/// Parsed DSL document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub statements: Vec<Stmt>,
}

/// A single statement in the DSL (or prose when using `parse_full`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    Item {
        title: String,
        body: Option<String>,
    },
    Vote {
        item1: String,
        item2: String,
        ratio_left: i32,
        ratio_right: i32,
        /// Required non-empty explanation (from leading `{ ... }`).
        explanation: String,
    },
    Prose {
        text: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum DslError {
    #[error("parse error: {0}")]
    Parse(String),
}

/// Greatest common divisor (non-negative). `gcd(0, n) == n`, `gcd(0, 0) == 0`.
pub fn gcd_i32(mut a: i32, mut b: i32) -> i32 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Reduce a vote ratio by dividing both sides by their GCD.
/// Non-positive inputs are clamped to 0; `(0, 0)` becomes `(1, 1)`.
pub fn reduce_ratio(left: i32, right: i32) -> (i32, i32) {
    let l = left.max(0);
    let r = right.max(0);
    if l == 0 && r == 0 {
        return (1, 1);
    }
    let g = gcd_i32(l, r).max(1);
    (l / g, r / g)
}

/// Helper to mask balanced blocks to protect them during filtering/parsing.
///
/// Matches the legacy Python parser behavior:
/// - Supports toggle markers (open == close), e.g. ```...```
/// - Supports nested markers (open != close), e.g. { ... { ... } ... }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    CodeFence,
    DoubleBrace,
    Brace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaskedBlock {
    kind: BlockKind,
}

#[derive(Debug, Default, Clone)]
pub struct BlockMasker {
    pub replacements: HashMap<String, String>,
    blocks: HashMap<String, MaskedBlock>,
    next_id: u32,
}

impl BlockMasker {
    pub fn new() -> Self {
        Self {
            replacements: HashMap::new(),
            blocks: HashMap::new(),
            next_id: 0,
        }
    }

    fn new_token(&mut self, haystack: &str) -> String {
        loop {
            let token = format!("__BLOCK_{:08x}__", self.next_id);
            self.next_id = self.next_id.wrapping_add(1);
            if !self.replacements.contains_key(&token) && !haystack.contains(&token) {
                return token;
            }
        }
    }

    /// Replace outermost balanced blocks with tokens.
    pub fn mask(&mut self, text: &str, open_marker: &str, close_marker: &str) -> String {
        self.mask_kind(text, open_marker, close_marker, BlockKind::Brace)
    }

    /// Replace outermost balanced blocks with typed deterministic tokens.
    pub fn mask_kind(
        &mut self,
        text: &str,
        open_marker: &str,
        close_marker: &str,
        kind: BlockKind,
    ) -> String {
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
                    let token = self.new_token(text);
                    self.replacements
                        .insert(token.clone(), original_block.to_string());
                    self.blocks.insert(token.clone(), MaskedBlock { kind });
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

    pub fn block_kind(&self, token: &str) -> Option<BlockKind> {
        self.blocks.get(token).map(|b| b.kind)
    }
}

fn mask_all(mut masker: BlockMasker, text: &str) -> (BlockMasker, String) {
    // Mask hierarchy: Code -> Double Brace -> Single Brace.
    let t = masker.mask_kind(text, "```", "```", BlockKind::CodeFence);
    let t = masker.mask_kind(&t, "{{", "}}", BlockKind::DoubleBrace);
    let t = masker.mask_kind(&t, "{", "}", BlockKind::Brace);
    (masker, t)
}

fn mask_code_fences(mut masker: BlockMasker, text: &str) -> (BlockMasker, String) {
    let t = masker.mask_kind(text, "```", "```", BlockKind::CodeFence);
    (masker, t)
}

fn is_external_item_path_rest(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    name.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '_' | '-' | '/' | '.' | '?' | '=' | '&' | '%' | ':' | '#' | '+' | '@' | '~'
            )
    })
}

fn is_item_name(s: &str) -> bool {
    let mut name = s.trim();
    if let Some(rest) = name.strip_prefix("-/") {
        return is_external_item_path_rest(rest);
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProseToken {
    Text(String),
    ItemRef(String),
}

fn trim_prose_item_ref_end(s: &str, mut end: usize) -> usize {
    while end > 0 {
        let Some((idx, c)) = s[..end].char_indices().next_back() else {
            break;
        };
        if matches!(
            c,
            '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '"' | '\''
        ) {
            end = idx;
        } else {
            break;
        }
    }
    end
}

fn parse_item_name_at_with_mode(
    s: &str,
    i: usize,
    trim_trailing_punctuation: bool,
) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if i >= bytes.len() {
        return None;
    }

    // URL_REF: https://... or http://...
    if s[i..].starts_with("https://") || s[i..].starts_with("http://") {
        let mut j = i;
        while j < bytes.len() {
            if trim_trailing_punctuation && bytes[j] == b'\n' {
                break;
            }
            if bytes[j..].starts_with(b"__BLOCK_") || is_ws_byte(bytes[j]) {
                break;
            }
            j += 1;
        }
        if j <= i {
            return None;
        }
        if trim_trailing_punctuation {
            j = trim_prose_item_ref_end(s, j);
            if j <= i {
                return None;
            }
        }
        return Some((s[i..j].to_string(), j));
    }

    // External ontology: "-/" HOST "/..." (mirrors ~/ lexer)
    if bytes.get(i..i + 2) == Some(b"-/") {
        let mut j = i + 2;
        while j < bytes.len() {
            if bytes[j..].starts_with(b"__BLOCK_") {
                break;
            }
            let c = bytes[j] as char;
            if c.is_ascii_alphanumeric()
                || matches!(
                    c,
                    '_' | '-' | '/' | '.' | '?' | '=' | '&' | '%' | ':' | '#' | '+' | '@' | '~'
                )
            {
                j += 1;
                continue;
            }
            break;
        }
        if j <= i + 2 {
            return None;
        }
        if trim_trailing_punctuation {
            j = trim_prose_item_ref_end(s, j);
            if j <= i + 2 {
                return None;
            }
        }
        let raw = &s[i..j];
        if !is_item_name(raw) {
            return None;
        }
        return Some((raw.to_string(), j));
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
    Some((format!("~/{name}"), j))
}

fn parse_item_name_at(s: &str, i: usize) -> Option<(String, usize)> {
    parse_item_name_at_with_mode(s, i, false)
}

pub fn parse_prose_item_ref_at(s: &str, i: usize) -> Option<(String, usize)> {
    parse_item_name_at_with_mode(s, i, true)
}

pub fn tokenize_prose_item_refs(text: &str) -> Vec<ProseToken> {
    if text.is_empty() {
        return Vec::new();
    }
    let (masker, masked) = mask_code_fences(BlockMasker::new(), text);
    let mut tokens = Vec::new();
    let mut text_start = 0usize;
    let mut i = 0usize;

    while i < masked.len() {
        if let Some((raw, end)) = parse_prose_item_ref_at(&masked, i) {
            if text_start < i {
                tokens.push(ProseToken::Text(masker.unmask(&masked[text_start..i])));
            }
            tokens.push(ProseToken::ItemRef(masker.unmask(&raw)));
            i = end;
            text_start = i;
            continue;
        }

        let Some((_, c)) = masked[i..].char_indices().next() else {
            break;
        };
        i += c.len_utf8();
    }

    if text_start < masked.len() {
        tokens.push(ProseToken::Text(masker.unmask(&masked[text_start..])));
    }
    tokens
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

fn parse_block_prefixed_statement(
    block_token: &str,
    tail: &str,
    masker: &BlockMasker,
) -> Result<Stmt, DslError> {
    if masker.block_kind(block_token) == Some(BlockKind::CodeFence) {
        return Err(DslError::Parse(
            "vote explanations must use `{ ... }`; code fences belong inside body blocks"
                .to_string(),
        ));
    }
    // vote: block item_ref comparison item_ref
    let s = tail.trim_start();
    if s.is_empty() {
        return Err(DslError::Parse(
            "missing vote statement after leading explanation block".to_string(),
        ));
    }

    let (item1, j) =
        parse_item_name_at(s, 0).ok_or_else(|| DslError::Parse("invalid item name".to_string()))?;
    let explanation = masker.extract_body(block_token);
    let mut i = skip_ws(s, j);

    if i >= s.len() {
        return Err(DslError::Parse(
            "leading `{ ... }` blocks are vote explanations; item bodies belong after item paths"
                .to_string(),
        ));
    }

    let ((ratio_left, ratio_right), k) = parse_comparison_at(s, i)
        .ok_or_else(|| DslError::Parse(format!("invalid comparison near: {}", &s[i..])))?;
    if ratio_left == 0 || ratio_right == 0 {
        return Err(DslError::Parse(
            "vote ratio sides must be ≥ 1; use 1:1 for a tie or omit the vote".to_string(),
        ));
    }
    if ratio_left > 100 || ratio_right > 100 {
        return Err(DslError::Parse(
            "vote ratio sides must be ≤ 100".to_string(),
        ));
    }
    let (ratio_left, ratio_right) = reduce_ratio(ratio_left, ratio_right);
    i = skip_ws(s, k);
    let (item2, m) = parse_item_name_at(s, i)
        .ok_or_else(|| DslError::Parse("invalid rhs item name".to_string()))?;
    i = skip_ws(s, m);
    if explanation.trim().is_empty() {
        return Err(DslError::Parse("empty vote explanation".to_string()));
    }
    let tail = s[i..].trim();
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

fn parse_item_definition_statement(stripped: &str, masker: &BlockMasker) -> Result<Stmt, DslError> {
    let (item1, j) = parse_item_name_at(stripped, 0)
        .ok_or_else(|| DslError::Parse("invalid item name".to_string()))?;
    let i = skip_ws(stripped, j);

    if i >= stripped.len() {
        return Ok(Stmt::Item {
            title: item1,
            body: None,
        });
    }

    if let Some((tok, end)) = parse_block_token_at(stripped, i) {
        if masker.block_kind(&tok) == Some(BlockKind::CodeFence) {
            return Err(DslError::Parse(
                "item bodies must use `{ ... }`; code fences belong inside body blocks".to_string(),
            ));
        }
        let body = masker.extract_body(&tok);
        let tail = stripped[end..].trim();
        if !tail.is_empty() {
            return Err(DslError::Parse("extra tokens after item".to_string()));
        }
        return Ok(Stmt::Item {
            title: item1,
            body: Some(body),
        });
    }

    Err(DslError::Parse(
        "vote explanations must start with a `{ ... }` block before the comparison".to_string(),
    ))
}

fn parse_line(masked_line: &str, masker: &BlockMasker) -> Result<Vec<Stmt>, DslError> {
    let stripped = masked_line.trim_start();
    if stripped.is_empty() {
        return Ok(vec![]);
    }
    let first = stripped.chars().next().unwrap();
    match first {
        '#' => Err(DslError::Parse("not a DSL line".to_string())),
        ':' => Err(DslError::Parse("leading ':' is not supported".to_string())),
        '@' => Err(DslError::Parse("not a DSL line".to_string())),
        '_' => {
            let Some((tok, end)) = parse_block_token_at(stripped, 0) else {
                return Err(DslError::Parse("not a DSL line".to_string()));
            };
            parse_block_prefixed_statement(&tok, &stripped[end..], masker).map(|stmt| vec![stmt])
        }
        '/' => Err(DslError::Parse(
            "item paths must use `~/` (e.g. `~/languages/python`), not a leading `/`".to_string(),
        )),
        '~' => Ok(vec![parse_item_definition_statement(stripped, masker)?]),
        'h' => {
            if stripped.starts_with("https://") || stripped.starts_with("http://") {
                Ok(vec![parse_item_definition_statement(stripped, masker)?])
            } else {
                Err(DslError::Parse("not a DSL line".to_string()))
            }
        }
        '-' => {
            if stripped.starts_with("-/") {
                Ok(vec![parse_item_definition_statement(stripped, masker)?])
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
    let mut pending_block: Option<String> = None;

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
        if let Some(tok) = pending_block.as_ref() {
            if stripped.is_empty() {
                continue;
            }
            if stripped.starts_with("-/")
                || stripped.starts_with("~/")
                || stripped.starts_with("https://")
                || stripped.starts_with("http://")
            {
                statements.push(parse_block_prefixed_statement(tok, stripped, &masker)?);
                pending_block = None;
                continue;
            }
            return Err(DslError::Parse(
                "expected vote statement after leading explanation block".to_string(),
            ));
        }

        if !stripped.is_empty()
            && (stripped.starts_with("-/")
                || {
                    let c = stripped.chars().next().unwrap();
                    ":/!~_".contains(c)
                }
                || stripped.starts_with("https://")
                || stripped.starts_with("http://"))
        {
            // Flush prose buffer first
            flush_prose(&mut prose_buffer, &mut statements, &masker);

            if let Some((tok, end)) = parse_block_token_at(stripped, 0) {
                if stripped[end..].trim().is_empty() {
                    if masker.block_kind(&tok) == Some(BlockKind::CodeFence) {
                        prose_buffer.push(line);
                        continue;
                    }
                    pending_block = Some(tok);
                    continue;
                }
            }

            // Parse DSL line; DSL statements are not prose, so errors should propagate.
            statements.extend(parse_line(line, &masker)?);
        } else {
            prose_buffer.push(line);
        }
    }

    if pending_block.is_some() {
        return Err(DslError::Parse(
            "missing vote statement after leading explanation block".to_string(),
        ));
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
    fn blockmasker_tokens_are_deterministic_and_typed() {
        let input = "x ```code``` y {body}";
        let (masker, masked) = mask_all(BlockMasker::new(), input);
        assert!(masked.contains("__BLOCK_00000000__"));
        assert!(masked.contains("__BLOCK_00000001__"));
        assert_eq!(
            masker.block_kind("__BLOCK_00000000__"),
            Some(BlockKind::CodeFence)
        );
        assert_eq!(
            masker.block_kind("__BLOCK_00000001__"),
            Some(BlockKind::Brace)
        );
        assert_eq!(masker.unmask(&masked), input);
    }

    #[test]
    fn prose_tokenizer_finds_tilde_dash_and_raw_url_refs() {
        let tokens =
            tokenize_prose_item_refs("see ~/a/b then -/example.com/x and https://Example.com/A/B.");
        assert_eq!(
            tokens,
            vec![
                ProseToken::Text("see ".to_string()),
                ProseToken::ItemRef("~/a/b".to_string()),
                ProseToken::Text(" then ".to_string()),
                ProseToken::ItemRef("-/example.com/x".to_string()),
                ProseToken::Text(" and ".to_string()),
                ProseToken::ItemRef("https://Example.com/A/B".to_string()),
                ProseToken::Text(".".to_string()),
            ]
        );
    }

    #[test]
    fn prose_tokenizer_stops_raw_urls_at_newlines() {
        let tokens = tokenize_prose_item_refs("https://example.com/a/b.\n-/example.com/a/b");
        assert_eq!(
            tokens,
            vec![
                ProseToken::ItemRef("https://example.com/a/b".to_string()),
                ProseToken::Text(".\n".to_string()),
                ProseToken::ItemRef("-/example.com/a/b".to_string()),
            ]
        );
    }

    #[test]
    fn prose_tokenizer_does_not_linkify_inside_code_fences() {
        let tokens = tokenize_prose_item_refs(
            "before ```json\n{\"url\":\"https://example.com\"}\n``` after ~/x",
        );
        assert_eq!(
            tokens,
            vec![
                ProseToken::Text(
                    "before ```json\n{\"url\":\"https://example.com\"}\n``` after ".to_string()
                ),
                ProseToken::ItemRef("~/x".to_string()),
            ]
        );
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
    fn parse_item_with_braced_fenced_json_body_preserves_braces() {
        let input = "~/item/in/url {\n```json\n{\"test\": true}\n```\n}";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Item {
                title: "~/item/in/url".to_string(),
                body: Some("```json\n{\"test\": true}\n```".to_string()),
            }]
        );
    }

    #[test]
    fn parse_rejects_singleton_fenced_json_item_body() {
        let input = "~/item/in/url ```json\n{\"test\": true}\n```";
        let err = parse_full(input).unwrap_err().to_string();
        assert!(
            err.contains("item bodies must use"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_keeps_standalone_code_fence_as_prose() {
        let input = "```json\n{\"test\": true}\n```";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Prose {
                text: input.to_string(),
            }]
        );
    }

    #[test]
    fn parse_rejects_code_fence_vote_explanation() {
        let input = "```json\n{\"why\": true}\n```\n~/a 2:1 ~/b";
        let err = parse_full(input).unwrap_err().to_string();
        assert!(
            err.contains("vote explanations must start"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_raw_url_item_with_braced_fenced_json_body() {
        let input = "https://example.com/itembody/slug {\n```json\n{\"test\": true}\n```\n}";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Item {
                title: "https://example.com/itembody/slug".to_string(),
                body: Some("```json\n{\"test\": true}\n```".to_string()),
            }]
        );
    }

    #[test]
    fn parse_vote_ratio_and_symbols() {
        let d1 = parse_full("{because}\n~/a 3:1 ~/b").unwrap();
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

        let d2 = parse_full("{because}\n~/a > ~/b").unwrap();
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

        let d3 = parse_full("{because}\n~/a = ~/b").unwrap();
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
    fn parse_vote_rejects_zero_zero_ratio() {
        let err = parse_full("{tie placeholder}\n~/a 0:0 ~/b").unwrap_err();
        let DslError::Parse(msg) = err;
        assert!(
            msg.contains("≥ 1"),
            "expected zero-side rejection message, got: {msg}"
        );
    }

    #[test]
    fn parse_vote_rejects_left_zero_ratio() {
        let err = parse_full("{prefer b}\n~/a 0:5 ~/b").unwrap_err();
        let DslError::Parse(msg) = err;
        assert!(
            msg.contains("≥ 1"),
            "expected zero-side rejection message, got: {msg}"
        );
    }

    #[test]
    fn parse_vote_rejects_right_zero_ratio() {
        let err = parse_full("{prefer a}\n~/a 5:0 ~/b").unwrap_err();
        let DslError::Parse(msg) = err;
        assert!(
            msg.contains("≥ 1"),
            "expected zero-side rejection message, got: {msg}"
        );
    }

    #[test]
    fn parse_vote_rejects_over_max_ratio() {
        let err = parse_full("{prefer a strongly}\n~/a 101:1 ~/b").unwrap_err();
        let DslError::Parse(msg) = err;
        assert!(
            msg.contains("≤ 100"),
            "expected max ratio rejection message, got: {msg}"
        );
    }

    #[test]
    fn parse_vote_accepts_max_ratio() {
        let doc = parse_full("{prefer a}\n~/a 100:1 ~/b").unwrap();
        assert!(matches!(
            doc.statements.last(),
            Some(Stmt::Vote { ratio_left: 100, ratio_right: 1, .. })
        ));
    }

    #[test]
    fn reduce_ratio_divides_by_gcd() {
        assert_eq!(reduce_ratio(50, 50), (1, 1));
        assert_eq!(reduce_ratio(25, 75), (1, 3));
        assert_eq!(reduce_ratio(75, 25), (3, 1));
        assert_eq!(reduce_ratio(2, 1), (2, 1));
        assert_eq!(reduce_ratio(100, 1), (100, 1));
        assert_eq!(reduce_ratio(0, 0), (1, 1));
    }

    #[test]
    fn parse_vote_reduces_ratio_by_gcd() {
        let doc = parse_full("{tie}\n~/a 50:50 ~/b").unwrap();
        assert!(matches!(
            doc.statements.last(),
            Some(Stmt::Vote {
                ratio_left: 1,
                ratio_right: 1,
                ..
            })
        ));
        let doc = parse_full("{prefer b}\n~/a 25:75 ~/b").unwrap();
        assert!(matches!(
            doc.statements.last(),
            Some(Stmt::Vote {
                ratio_left: 1,
                ratio_right: 3,
                ..
            })
        ));
    }

    #[test]
    fn parse_full_interleaves_prose() {
        let input = "hello\n#tag\nworld";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Prose {
                text: "hello\n#tag\nworld".to_string()
            }]
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
    fn parse_vote_with_attached_explanation_without_space() {
        let input = "{because}~/a 2:1 ~/b";
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
        let input = "{ because }\n~/whitepaper/a 3:1 ~/whitepaper/b";
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
    fn parse_rejects_leading_colon() {
        let inputs = [":beauty", ":x", ":"];
        for input in &inputs {
            let result = parse_full(input);
            assert!(result.is_err(), "expected parse error for {input:?}");
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("leading ':' is not supported"),
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
        assert!(
            err_msg.contains("vote explanations must start"),
            "error: {}",
            err_msg
        );
    }

    #[test]
    fn parse_full_rejects_legacy_trailing_explanation_vote() {
        let result = parse_full("~/a 2:1 ~/b {because}");
        assert!(result.is_err(), "legacy vote syntax should fail");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("vote explanations must start"),
            "error: {}",
            err_msg
        );
    }

    #[test]
    fn parse_full_rejects_block_first_item_body() {
        let result = parse_full("{body}\n~/a");
        assert!(result.is_err(), "block-first item body syntax should fail");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("item bodies belong after item paths"),
            "error: {}",
            err_msg
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
        let input = "{ because }\nhttps://slug.social/~/music/a 3:1 https://slug.social/~/music/b";
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
