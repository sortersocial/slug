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
        aspect: Option<String>,
    },
    Aspect {
        slug: Option<String>,
        prompt: Option<String>,
    },
    /// Weighted containment (`<:`) or border (`!<:`) claim.
    ///
    /// Explicit claims carry a required explanation (like rank votes). Path-desugared
    /// sugar edges set `sugar: true` and `explanation: None`.
    Containment {
        child: String,
        parent: String,
        /// `true` for `!<:` (non-membership border).
        border: bool,
        explanation: Option<String>,
        sugar: bool,
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

fn is_path_segment(seg: &str) -> bool {
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
    true
}

fn is_item_name(s: &str) -> bool {
    let mut name = s.trim();
    if let Some(rest) = name.strip_prefix("-/") {
        return is_external_item_path_rest(rest);
    }
    if let Some(rest) = name.strip_prefix("~/") {
        name = rest;
    } else if let Some(rest) = name.strip_prefix('~') {
        return !rest.is_empty() && !rest.contains('/') && is_path_segment(rest);
    } else if let Some(rest) = name.strip_prefix('/') {
        name = rest;
    }
    if name.is_empty() {
        return false;
    }
    name.split('/').all(is_path_segment)
}

/// Compile-time path sugar: `~/a/b/c` → leaf `~c` plus idempotent edges
/// `c <: b`, `b <: a`, `a <: ~` (root). URL / `-/` refs stay atomic.
/// Bare `~name` is already a leaf and yields no edges.
pub fn desugar_item_ref(raw: &str) -> (String, Vec<(String, String)>) {
    let raw = raw.trim();
    if raw.starts_with("http://") || raw.starts_with("https://") || raw.starts_with("-/") {
        return (raw.to_string(), vec![]);
    }
    if raw == "~/" || raw == "~" {
        return ("~/".to_string(), vec![]);
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let segs: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            return ("~/".to_string(), vec![]);
        }
        let leaf = format!("~{}", segs[segs.len() - 1]);
        let mut edges = Vec::new();
        let mut child = leaf.clone();
        for i in (0..segs.len() - 1).rev() {
            let parent = format!("~{}", segs[i]);
            edges.push((child, parent.clone()));
            child = parent;
        }
        edges.push((child, "~/".to_string()));
        return (leaf, edges);
    }
    if raw.starts_with('~') && !raw[1..].contains('/') {
        return (raw.to_string(), vec![]);
    }
    (raw.to_string(), vec![])
}

fn sugar_containments(raw: &str) -> (String, Vec<Stmt>) {
    let (leaf, edges) = desugar_item_ref(raw);
    let stmts = edges
        .into_iter()
        .map(|(child, parent)| Stmt::Containment {
            child,
            parent,
            border: false,
            explanation: None,
            sugar: true,
        })
        .collect();
    (leaf, stmts)
}

pub fn is_valid_aspect_slug(s: &str) -> bool {
    let len = s.len();
    (1..=64).contains(&len)
        && s.bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

fn try_parse_aspect_line(stripped: &str, masker: &BlockMasker) -> Option<Stmt> {
    if !stripped.starts_with(':') {
        return None;
    }
    let rest = stripped[1..].trim_end();
    if rest.is_empty() {
        return Some(Stmt::Aspect {
            slug: None,
            prompt: None,
        });
    }
    let bytes = rest.as_bytes();
    let mut slug_len = 0usize;
    while slug_len < bytes.len() {
        if bytes[slug_len..].starts_with(b"__BLOCK_") {
            break;
        }
        if !matches!(bytes[slug_len], b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-') {
            break;
        }
        slug_len += 1;
        if slug_len > 64 {
            return None;
        }
    }
    if slug_len == 0 {
        return None;
    }
    if slug_len == 64 {
        if let Some(tail) = bytes.get(64..) {
            if !tail.starts_with(b"__BLOCK_")
                && tail
                    .first()
                    .is_some_and(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
            {
                return None;
            }
        }
    }
    let slug = &rest[..slug_len];
    if !is_valid_aspect_slug(slug) {
        return None;
    }
    let after = rest[slug_len..].trim_start();
    if after.is_empty() {
        return Some(Stmt::Aspect {
            slug: Some(slug.to_string()),
            prompt: None,
        });
    }
    let (tok, end) = parse_block_token_at(after, 0)?;
    if masker.block_kind(&tok) == Some(BlockKind::CodeFence) {
        return None;
    }
    if !after[end..].trim().is_empty() {
        return None;
    }
    Some(Stmt::Aspect {
        slug: Some(slug.to_string()),
        prompt: Some(masker.extract_body(&tok)),
    })
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

    // ITEM_REF: "~/" ITEM_NAME, bare "~" TOKEN, or https?:// URL
    // Leading `/path` alone is not valid — use `~/path` or `~name` for ontology items.
    let mut j = i;
    if bytes[j] != b'~' {
        return None;
    }
    j += 1;
    if j < bytes.len() && bytes[j] == b'/' {
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
        return Some((format!("~/{name}"), j));
    }
    // Bare tilde token: `~name` (one path segment, no slash).
    let start = j;
    while j < bytes.len() {
        if bytes[j..].starts_with(b"__BLOCK_") {
            break;
        }
        let c = bytes[j] as char;
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            j += 1;
            continue;
        }
        break;
    }
    if j <= start {
        return None;
    }
    let name = &s[start..j];
    if !is_path_segment(name) {
        return None;
    }
    Some((format!("~{name}"), j))
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

/// `<:` (membership) or `!<:` (border). Must be checked before rank-vote `<`.
fn parse_containment_op_at(s: &str, i: usize) -> Option<(bool, usize)> {
    let rest = s.get(i..)?;
    if rest.starts_with("!<:") {
        return Some((true, i + 3));
    }
    if rest.starts_with("<:") {
        return Some((false, i + 2));
    }
    None
}

fn parse_comparison_at(s: &str, i: usize) -> Option<((i32, i32), usize)> {
    let bytes = s.as_bytes();
    if i >= bytes.len() {
        return None;
    }
    if bytes[i] == b'>' {
        return Some(((2, 1), i + 1));
    }
    // Digraph `<:` is containment, not the rank-vote shorthand `<`.
    if bytes[i] == b'<' && bytes.get(i + 1) != Some(&b':') {
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
) -> Result<Vec<Stmt>, DslError> {
    if masker.block_kind(block_token) == Some(BlockKind::CodeFence) {
        return Err(DslError::Parse(
            "vote explanations must use `{ ... }`; code fences belong inside body blocks"
                .to_string(),
        ));
    }
    // vote / containment: block item_ref (comparison | <: | !<:) item_ref
    let s = tail.trim_start();
    if s.is_empty() {
        return Err(DslError::Parse(
            "missing vote statement after leading explanation block".to_string(),
        ));
    }

    let (item1_raw, j) =
        parse_item_name_at(s, 0).ok_or_else(|| DslError::Parse("invalid item name".to_string()))?;
    let explanation = masker.extract_body(block_token);
    let mut i = skip_ws(s, j);

    if i >= s.len() {
        return Err(DslError::Parse(
            "leading `{ ... }` blocks are vote explanations; item bodies belong after item paths"
                .to_string(),
        ));
    }

    if explanation.trim().is_empty() {
        return Err(DslError::Parse("empty vote explanation".to_string()));
    }

    if let Some((border, k)) = parse_containment_op_at(s, i) {
        i = skip_ws(s, k);
        let Some((item2_raw, m)) = parse_item_name_at(s, i) else {
            return Err(DslError::Parse("not a DSL line".to_string()));
        };
        i = skip_ws(s, m);
        if !s[i..].trim().is_empty() {
            return Err(DslError::Parse("not a DSL line".to_string()));
        }
        let (child, mut stmts) = sugar_containments(&item1_raw);
        let (parent, parent_sugar) = sugar_containments(&item2_raw);
        stmts.extend(parent_sugar);
        stmts.push(Stmt::Containment {
            child,
            parent,
            border,
            explanation: Some(explanation),
            sugar: false,
        });
        return Ok(stmts);
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
    let (item2_raw, m) = parse_item_name_at(s, i)
        .ok_or_else(|| DslError::Parse("invalid rhs item name".to_string()))?;
    i = skip_ws(s, m);
    let extra = s[i..].trim();
    if !extra.is_empty() {
        return Err(DslError::Parse("extra tokens after vote".to_string()));
    }

    let (item1, mut stmts) = sugar_containments(&item1_raw);
    let (item2, rhs_sugar) = sugar_containments(&item2_raw);
    stmts.extend(rhs_sugar);
    stmts.push(Stmt::Vote {
        item1,
        item2,
        ratio_left,
        ratio_right,
        explanation,
        aspect: None,
    });
    Ok(stmts)
}

fn parse_item_definition_statement(
    stripped: &str,
    masker: &BlockMasker,
) -> Result<Vec<Stmt>, DslError> {
    let (item1_raw, j) = parse_item_name_at(stripped, 0)
        .ok_or_else(|| DslError::Parse("invalid item name".to_string()))?;
    let i = skip_ws(stripped, j);

    if i >= stripped.len() {
        let (title, mut stmts) = sugar_containments(&item1_raw);
        stmts.push(Stmt::Item { title, body: None });
        return Ok(stmts);
    }

    if parse_containment_op_at(stripped, i).is_some() {
        // Complete `~a <: ~b` without explanation is an error (like a rank vote).
        // Incomplete / extra-token forms fall back to prose.
        let (_, k) = parse_containment_op_at(stripped, i).unwrap();
        let after = skip_ws(stripped, k);
        match parse_item_name_at(stripped, after) {
            None => return Err(DslError::Parse("not a DSL line".to_string())),
            Some((_, m)) if !stripped[skip_ws(stripped, m)..].trim().is_empty() => {
                return Err(DslError::Parse("not a DSL line".to_string()));
            }
            Some(_) => {
                return Err(DslError::Parse(
                    "containment claims require a leading `{ ... }` explanation block".to_string(),
                ));
            }
        }
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
        let (title, mut stmts) = sugar_containments(&item1_raw);
        stmts.push(Stmt::Item {
            title,
            body: Some(body),
        });
        return Ok(stmts);
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
        ':' => try_parse_aspect_line(stripped, masker)
            .map(|s| vec![s])
            .ok_or_else(|| DslError::Parse("not a DSL line".to_string())),
        '@' => Err(DslError::Parse("not a DSL line".to_string())),
        '_' => {
            let Some((tok, end)) = parse_block_token_at(stripped, 0) else {
                return Err(DslError::Parse("not a DSL line".to_string()));
            };
            parse_block_prefixed_statement(&tok, &stripped[end..], masker)
        }
        '/' => Err(DslError::Parse(
            "item paths must use `~/` (e.g. `~/languages/python`), not a leading `/`".to_string(),
        )),
        '~' => parse_item_definition_statement(stripped, masker),
        'h' => {
            if stripped.starts_with("https://") || stripped.starts_with("http://") {
                parse_item_definition_statement(stripped, masker)
            } else {
                Err(DslError::Parse("not a DSL line".to_string()))
            }
        }
        '-' => {
            if stripped.starts_with("-/") {
                parse_item_definition_statement(stripped, masker)
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
    let mut current_aspect: Option<String> = None;

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
                || stripped.starts_with('~')
                || stripped.starts_with("https://")
                || stripped.starts_with("http://")
            {
                let start = statements.len();
                match parse_block_prefixed_statement(tok, stripped, &masker) {
                    Ok(stmts) => {
                        statements.extend(stmts);
                        for stmt in &mut statements[start..] {
                            if let Stmt::Vote { aspect, .. } = stmt {
                                *aspect = current_aspect.clone();
                            }
                        }
                        pending_block = None;
                        continue;
                    }
                    Err(DslError::Parse(msg)) if msg == "not a DSL line" => {
                        return Err(DslError::Parse(
                            "expected vote statement after leading explanation block".to_string(),
                        ));
                    }
                    Err(e) => return Err(e),
                }
            }
            return Err(DslError::Parse(
                "expected vote statement after leading explanation block".to_string(),
            ));
        }

        if let Some(aspect_stmt) = try_parse_aspect_line(stripped, &masker) {
            flush_prose(&mut prose_buffer, &mut statements, &masker);
            if let Stmt::Aspect { slug, .. } = &aspect_stmt {
                current_aspect = slug.clone();
            }
            statements.push(aspect_stmt);
            continue;
        }

        if !stripped.is_empty()
            && (stripped.starts_with("-/")
                || {
                    let c = stripped.chars().next().unwrap();
                    "/!~_".contains(c)
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

            // Parse DSL line; DSL statements are not prose, so errors should propagate
            // except malformed containment forms (`~a <:`, `~a <: ~b extra`) which fall
            // back to prose — they were never a successful historical parse.
            let start = statements.len();
            match parse_line(line, &masker) {
                Ok(stmts) => {
                    statements.extend(stmts);
                    for stmt in &mut statements[start..] {
                        if let Stmt::Vote { aspect, .. } = stmt {
                            *aspect = current_aspect.clone();
                        }
                    }
                }
                Err(DslError::Parse(msg)) if msg == "not a DSL line" => {
                    prose_buffer.push(line);
                }
                Err(e) => return Err(e),
            }
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

    fn items(doc: &Document) -> Vec<&Stmt> {
        doc.statements
            .iter()
            .filter(|s| matches!(s, Stmt::Item { .. }))
            .collect()
    }

    fn votes(doc: &Document) -> Vec<&Stmt> {
        doc.statements
            .iter()
            .filter(|s| matches!(s, Stmt::Vote { .. }))
            .collect()
    }

    fn containments(doc: &Document) -> Vec<&Stmt> {
        doc.statements
            .iter()
            .filter(|s| matches!(s, Stmt::Containment { .. }))
            .collect()
    }

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
            items(&doc),
            vec![&Stmt::Item {
                title: "~rust".to_string(),
                body: Some("Systems language".to_string()),
            }]
        );
        assert!(containments(&doc).iter().any(|s| matches!(
            s,
            Stmt::Containment {
                child,
                parent,
                sugar: true,
                border: false,
                ..
            } if child == "~rust" && parent == "~/"
        )));
    }

    #[test]
    fn parse_item_with_braced_fenced_json_body_preserves_braces() {
        let input = "~/item/in/url {\n```json\n{\"test\": true}\n```\n}";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            items(&doc),
            vec![&Stmt::Item {
                title: "~url".to_string(),
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
            votes(&d1),
            vec![&Stmt::Vote {
                item1: "~a".to_string(),
                item2: "~b".to_string(),
                ratio_left: 3,
                ratio_right: 1,
                explanation: "because".to_string(),
                aspect: None,
            }]
        );

        let d2 = parse_full("{because}\n~/a > ~/b").unwrap();
        assert_eq!(
            votes(&d2),
            vec![&Stmt::Vote {
                item1: "~a".to_string(),
                item2: "~b".to_string(),
                ratio_left: 2,
                ratio_right: 1,
                explanation: "because".to_string(),
                aspect: None,
            }]
        );

        let d3 = parse_full("{because}\n~/a = ~/b").unwrap();
        assert_eq!(
            votes(&d3),
            vec![&Stmt::Vote {
                item1: "~a".to_string(),
                item2: "~b".to_string(),
                ratio_left: 1,
                ratio_right: 1,
                explanation: "because".to_string(),
                aspect: None,
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
            Some(Stmt::Vote {
                ratio_left: 100,
                ratio_right: 1,
                ..
            })
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
            items(&doc),
            vec![&Stmt::Item {
                title: "~arrived".to_string(),
                body: Some("I had arrived.".to_string()),
            }]
        );
    }

    #[test]
    fn parse_vote_with_attached_explanation_without_space() {
        let input = "{because}~/a 2:1 ~/b";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            votes(&doc),
            vec![&Stmt::Vote {
                item1: "~a".to_string(),
                item2: "~b".to_string(),
                ratio_left: 2,
                ratio_right: 1,
                explanation: "because".to_string(),
                aspect: None,
            }]
        );
    }

    #[test]
    fn parse_nested_path_item() {
        let input = "~/whitepaper/architectural-choices { Body }";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            items(&doc),
            vec![&Stmt::Item {
                title: "~architectural-choices".to_string(),
                body: Some("Body".to_string()),
            }]
        );
        let sugars: Vec<(&str, &str)> = containments(&doc)
            .iter()
            .filter_map(|s| match s {
                Stmt::Containment {
                    child,
                    parent,
                    sugar: true,
                    ..
                } => Some((child.as_str(), parent.as_str())),
                _ => None,
            })
            .collect();
        assert!(sugars.contains(&("~architectural-choices", "~whitepaper")));
        assert!(sugars.contains(&("~whitepaper", "~/")));
    }

    #[test]
    fn parse_nested_path_vote() {
        let input = "{ because }\n~/whitepaper/a 3:1 ~/whitepaper/b";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            votes(&doc),
            vec![&Stmt::Vote {
                item1: "~a".to_string(),
                item2: "~b".to_string(),
                ratio_left: 3,
                ratio_right: 1,
                explanation: "because".to_string(),
                aspect: None,
            }]
        );
    }

    #[test]
    fn parse_aspect_declaration_and_prompt() {
        let d1 = parse_full(":beauty").unwrap();
        assert_eq!(
            d1.statements,
            vec![Stmt::Aspect {
                slug: Some("beauty".to_string()),
                prompt: None,
            }]
        );

        let d2 = parse_full(":beauty { winner is more beautiful }").unwrap();
        assert_eq!(
            d2.statements,
            vec![Stmt::Aspect {
                slug: Some("beauty".to_string()),
                prompt: Some("winner is more beautiful".to_string()),
            }]
        );

        let d3 = parse_full(":beauty{no space}").unwrap();
        assert_eq!(
            d3.statements,
            vec![Stmt::Aspect {
                slug: Some("beauty".to_string()),
                prompt: Some("no space".to_string()),
            }]
        );

        let d4 = parse_full(":a-b_c1").unwrap();
        assert_eq!(
            d4.statements,
            vec![Stmt::Aspect {
                slug: Some("a-b_c1".to_string()),
                prompt: None,
            }]
        );
    }

    #[test]
    fn parse_aspect_prompt_preserves_fenced_braces() {
        let input = ":beauty {\n```json\n{\"criterion\": true}\n```\n}";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Aspect {
                slug: Some("beauty".to_string()),
                prompt: Some("```json\n{\"criterion\": true}\n```".to_string()),
            }]
        );
    }

    #[test]
    fn parse_bare_colon_resets_aspect() {
        let doc = parse_full(":").unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Aspect {
                slug: None,
                prompt: None,
            }]
        );
        let doc = parse_full(":   ").unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Aspect {
                slug: None,
                prompt: None,
            }]
        );
    }

    #[test]
    fn parse_invalid_colon_lines_remain_prose() {
        let inputs = [
            ":)",
            ": note",
            "::x",
            ":UPPER",
            ":has space {x}",
            ":beauty extra",
            ":Beauty",
        ];
        for input in &inputs {
            let doc = parse_full(input).unwrap();
            assert_eq!(
                doc.statements,
                vec![Stmt::Prose {
                    text: input.to_string()
                }],
                "expected prose for {input:?}"
            );
        }
    }

    #[test]
    fn parse_invalid_colon_lines_merge_with_adjacent_prose() {
        let input = "hello\n:)\nworld";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Prose {
                text: "hello\n:)\nworld".to_string()
            }]
        );
    }

    #[test]
    fn parse_aspect_lexical_inheritance_across_votes_and_switches() {
        let input = "\
~/t/a {a}\n\
~/t/b {b}\n\
:beauty { more beautiful }\n\
{pretty}\n\
~/t/a 3:1 ~/t/b\n\
{also pretty}\n\
~/t/b 2:1 ~/t/a\n\
:\n\
{canonical}\n\
~/t/a 1:1 ~/t/b\n\
:speed\n\
{faster}\n\
~/t/a 4:1 ~/t/b\n";
        let doc = parse_full(input).unwrap();
        let votes: Vec<&Stmt> = doc
            .statements
            .iter()
            .filter(|s| matches!(s, Stmt::Vote { .. }))
            .collect();
        assert_eq!(votes.len(), 4);
        assert!(matches!(
            votes[0],
            Stmt::Vote {
                aspect: Some(a),
                ..
            } if a == "beauty"
        ));
        assert!(matches!(
            votes[1],
            Stmt::Vote {
                aspect: Some(a),
                ..
            } if a == "beauty"
        ));
        assert!(matches!(votes[2], Stmt::Vote { aspect: None, .. }));
        assert!(matches!(
            votes[3],
            Stmt::Vote {
                aspect: Some(a),
                ..
            } if a == "speed"
        ));
        let aspects: Vec<_> = doc
            .statements
            .iter()
            .filter_map(|s| match s {
                Stmt::Aspect { slug, prompt } => Some((slug.as_deref(), prompt.is_some())),
                _ => None,
            })
            .collect();
        assert_eq!(
            aspects,
            vec![
                (Some("beauty"), true),
                (None, false),
                (Some("speed"), false),
            ]
        );
    }

    #[test]
    fn parse_aspect_does_not_affect_item_definitions() {
        let input = ":beauty\n~/t/x {defined after aspect}";
        let doc = parse_full(input).unwrap();
        assert!(matches!(
            doc.statements.first(),
            Some(Stmt::Aspect {
                slug: Some(s),
                prompt: None,
            }) if s == "beauty"
        ));
        assert_eq!(
            items(&doc),
            vec![&Stmt::Item {
                title: "~x".to_string(),
                body: Some("defined after aspect".to_string()),
            }]
        );
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
            votes(&doc),
            vec![&Stmt::Vote {
                item1: "https://slug.social/~/music/a".to_string(),
                item2: "https://slug.social/~/music/b".to_string(),
                ratio_left: 3,
                ratio_right: 1,
                explanation: "because".to_string(),
                aspect: None,
            }]
        );
        assert!(
            containments(&doc).is_empty(),
            "URL items are never desugared"
        );
    }

    #[test]
    fn parse_bare_tilde_token_item() {
        let doc = parse_full("~luke { a jedi }").unwrap();
        assert_eq!(
            items(&doc),
            vec![&Stmt::Item {
                title: "~luke".to_string(),
                body: Some("a jedi".to_string()),
            }]
        );
        assert!(
            containments(&doc).is_empty(),
            "bare ~name is already a leaf; no path sugar"
        );
    }

    #[test]
    fn parse_bare_tilde_token_in_vote() {
        let doc = parse_full("{ because }\n~luke 2:1 ~vader").unwrap();
        assert_eq!(
            votes(&doc),
            vec![&Stmt::Vote {
                item1: "~luke".to_string(),
                item2: "~vader".to_string(),
                ratio_left: 2,
                ratio_right: 1,
                explanation: "because".to_string(),
                aspect: None,
            }]
        );
    }

    #[test]
    fn parse_explicit_containment_requires_explanation() {
        let err = parse_full("~a <: ~b").unwrap_err().to_string();
        assert!(
            err.contains("containment claims require"),
            "unexpected error: {err}"
        );
        let doc = parse_full("{ luke is a jedi }\n~luke <: ~jedi").unwrap();
        assert_eq!(
            containments(&doc)
                .iter()
                .filter(|s| matches!(s, Stmt::Containment { sugar: false, .. }))
                .count(),
            1
        );
        assert!(matches!(
            containments(&doc).iter().find(|s| matches!(s, Stmt::Containment { sugar: false, .. })),
            Some(Stmt::Containment {
                child,
                parent,
                border: false,
                explanation: Some(e),
                sugar: false,
            }) if child == "~luke" && parent == "~jedi" && e == "luke is a jedi"
        ));
    }

    #[test]
    fn parse_explicit_border_claim() {
        let doc = parse_full("{ not a jedi }\n~ahsoka !<: ~jedi").unwrap();
        assert!(matches!(
            containments(&doc).iter().find(|s| matches!(s, Stmt::Containment { sugar: false, .. })),
            Some(Stmt::Containment {
                child,
                parent,
                border: true,
                explanation: Some(e),
                sugar: false,
            }) if child == "~ahsoka" && parent == "~jedi" && e == "not a jedi"
        ));
    }

    #[test]
    fn parse_containment_desugars_path_sides() {
        let doc = parse_full("{ in }\n~/x/luke <: ~/y/jedi").unwrap();
        let claims = containments(&doc);
        let explicit = claims
            .iter()
            .find(|s| matches!(s, Stmt::Containment { sugar: false, .. }))
            .unwrap();
        assert!(matches!(
            explicit,
            Stmt::Containment {
                child,
                parent,
                border: false,
                sugar: false,
                ..
            } if child == "~luke" && parent == "~jedi"
        ));
        let sugars: Vec<(&str, &str)> = claims
            .iter()
            .filter_map(|s| match s {
                Stmt::Containment {
                    child,
                    parent,
                    sugar: true,
                    ..
                } => Some((child.as_str(), parent.as_str())),
                _ => None,
            })
            .collect();
        assert!(sugars.contains(&("~luke", "~x")));
        assert!(sugars.contains(&("~jedi", "~y")));
    }

    #[test]
    fn parse_containment_url_side_stays_atomic() {
        let doc = parse_full("{ src }\nhttps://example.com/a/b <: ~sources").unwrap();
        assert!(matches!(
            containments(&doc).iter().find(|s| matches!(s, Stmt::Containment { sugar: false, .. })),
            Some(Stmt::Containment {
                child,
                parent,
                ..
            }) if child == "https://example.com/a/b" && parent == "~sources"
        ));
    }

    #[test]
    fn parse_bare_less_than_is_still_rank_vote() {
        let doc = parse_full("{ prefer b }\n~a < ~b").unwrap();
        assert_eq!(
            votes(&doc),
            vec![&Stmt::Vote {
                item1: "~a".to_string(),
                item2: "~b".to_string(),
                ratio_left: 1,
                ratio_right: 2,
                explanation: "prefer b".to_string(),
                aspect: None,
            }]
        );
    }

    #[test]
    fn parse_equality_operator_is_not_implemented() {
        // `=` remains the rank-tie shorthand. `==` is extra tokens after that shorthand.
        let err = parse_full("{ same }\n~a == ~b").unwrap_err().to_string();
        assert!(
            err.contains("extra tokens after vote") || err.contains("invalid rhs item name"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn parse_containment_mid_prose_stays_prose() {
        let input = "the relation a <: b is discussed here";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Prose {
                text: input.to_string()
            }]
        );
    }

    #[test]
    fn parse_malformed_containment_falls_back_to_prose() {
        let incomplete = parse_full("~a <:").unwrap();
        assert_eq!(
            incomplete.statements,
            vec![Stmt::Prose {
                text: "~a <:".to_string()
            }]
        );
        let extra = parse_full("~a <: ~b extra").unwrap();
        assert_eq!(
            extra.statements,
            vec![Stmt::Prose {
                text: "~a <: ~b extra".to_string()
            }]
        );
    }

    #[test]
    fn parse_containment_inside_code_fence_stays_prose() {
        let input = "```\n~a <: ~b\n```";
        let doc = parse_full(input).unwrap();
        assert_eq!(
            doc.statements,
            vec![Stmt::Prose {
                text: input.to_string()
            }]
        );
    }

    #[test]
    fn prose_tokenizer_finds_bare_tilde_tokens() {
        let tokens = tokenize_prose_item_refs("see ~luke and ~/x/y.");
        assert_eq!(
            tokens,
            vec![
                ProseToken::Text("see ".to_string()),
                ProseToken::ItemRef("~luke".to_string()),
                ProseToken::Text(" and ".to_string()),
                ProseToken::ItemRef("~/x/y".to_string()),
                ProseToken::Text(".".to_string()),
            ]
        );
    }

    #[test]
    fn desugar_path_emits_idempotent_chain_including_root() {
        let (leaf, edges) = desugar_item_ref("~/a/b/c");
        assert_eq!(leaf, "~c");
        assert_eq!(
            edges,
            vec![
                ("~c".to_string(), "~b".to_string()),
                ("~b".to_string(), "~a".to_string()),
                ("~a".to_string(), "~/".to_string()),
            ]
        );
        let (leaf, edges) = desugar_item_ref("~luke");
        assert_eq!(leaf, "~luke");
        assert!(edges.is_empty());
        let (leaf, edges) = desugar_item_ref("https://example.com/a/b");
        assert_eq!(leaf, "https://example.com/a/b");
        assert!(edges.is_empty());
    }
}
