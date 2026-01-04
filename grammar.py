"""EmailDSL parser using Lark.

Parses email-based submissions with hashtags, items, votes, and attributes.
"""

import uuid
from dataclasses import dataclass
from typing import List, Optional, Dict

from lark import Lark, Transformer


# AST Node Definitions
@dataclass
class Hashtag:
    """Hashtag declaration: #ideas"""

    name: str


@dataclass
class Item:
    """Item submission: /item-title { optional body }"""

    title: str
    body: Optional[str] = None


@dataclass
class Attribute:
    """Attribute declaration: :difficulty :benefit"""

    name: str


@dataclass
class Vote:
    """Vote between items: /item1 10:1 /item2 { explanation }"""

    item1: str
    item2: str
    ratio_left: int
    ratio_right: int
    explanation: Optional[str] = None


@dataclass
class Email:
    """Email address: user@example.com"""

    address: str


@dataclass
class Prose:
    """Non-DSL text (preserved for rendering)"""

    text: str


@dataclass
class Document:
    """Parsed email document"""

    statements: List[object]


# Lark Grammar for EmailDSL
GRAMMAR = r"""
start: _NL* (statement _NL+)* statement?

?statement: hashtag
          | vote           // Try vote before item to avoid ambiguity
          | item
          | attribute_decl
          | email_address

hashtag: "#" hashtag_name
hashtag_name: ITEM_NAME

item: "/" item_ref body?

vote: "/" item_ref comparison "/" item_ref body?

comparison: NUMBER ":" NUMBER   -> ratio_comparison
          | ">"                 -> simple_greater
          | "<"                 -> simple_less
          | "="                 -> simple_equal

attribute_decl: attribute+
attribute: ":" WORD

email_address: EMAIL

body: BLOCK_TOKEN

item_ref: ITEM_NAME

// Terminals - order matters for priority
EMAIL: /[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}/

// Block token - represents masked content from BlockMasker
BLOCK_TOKEN: /__BLOCK_[a-f0-9]{8}__/

ITEM_NAME: /[a-zA-Z0-9_]+([-][a-zA-Z0-9_]+)*/
NUMBER: /[0-9]+/
WORD: /[a-zA-Z0-9_]+/

%import common.NEWLINE -> _NL
%import common.WS_INLINE
%ignore WS_INLINE
"""


class EmailDSLTransformer(Transformer):
    """Transform parse tree into AST nodes."""

    def __init__(self, masker: Optional['BlockMasker'] = None):
        """Initialize transformer with optional masker for unmasking block tokens."""
        super().__init__()
        self.masker = masker

    def start(self, children):
        # Filter out newlines and None values
        statements = [c for c in children if c is not None and not str(c).isspace()]
        return Document(statements=statements)

    def hashtag(self, children):
        return Hashtag(name=str(children[0]))

    def hashtag_name(self, words):
        return "".join(str(w) for w in words)

    def item(self, children):
        title = str(children[0])
        body = self._extract_body(children[1]) if len(children) > 1 else None
        return Item(title=title, body=body)

    def vote(self, children):
        item1 = str(children[0])
        comparison = children[1]
        item2 = str(children[2])
        explanation = self._extract_body(children[3]) if len(children) > 3 else None

        ratio_left, ratio_right = comparison

        return Vote(
            item1=item1,
            item2=item2,
            ratio_left=ratio_left,
            ratio_right=ratio_right,
            explanation=explanation,
        )

    def ratio_comparison(self, children):
        return (int(children[0]), int(children[1]))

    def simple_greater(self, children):
        return (2, 1)  # > means clearly better, 2:1 ratio

    def simple_less(self, children):
        return (1, 2)  # < means clearly worse, 1:2 ratio

    def simple_equal(self, children):
        return (1, 1)  # = means equal preference

    def attribute_decl(self, attributes):
        # Return list of attributes
        return [attr for attr in attributes]

    def attribute(self, children):
        return Attribute(name=str(children[0]))

    def email_address(self, children):
        return Email(address=str(children[0]))

    def item_ref(self, children):
        return str(children[0])

    def body(self, children):
        """Handle body block token - unmask and extract content."""
        token = str(children[0])
        if self.masker and token in self.masker.replacements:
            # Unmask the token to get original text with braces
            original = self.masker.replacements[token]
            # Remove outer braces and return content
            # Handle both {{ }} and { } formats
            if original.startswith("{{") and original.endswith("}}"):
                return original[2:-2].strip()
            elif original.startswith("{") and original.endswith("}"):
                return original[1:-1].strip()
            return original.strip()
        # If no masker or token not found, return as-is
        return token

    def _extract_body(self, body_token):
        """Extract text from body token."""
        if body_token is None:
            return None
        # body_token is already processed by body() method
        return str(body_token)


class BlockMasker:
    """Helper to mask balanced blocks to protect them during filtering."""

    def __init__(self):
        self.replacements: Dict[str, str] = {}

    def mask(self, text: str, open_marker: str, close_marker: str) -> str:
        """Replace outermost balanced blocks with tokens.

        Handles:
        1. Toggle markers (e.g. ``` ... ``` where open == close)
        2. Nested markers (e.g. { ... { ... } ... } where open != close)
        """
        if not text:
            return text

        result_parts = []
        current_idx = 0
        i = 0
        depth = 0
        start_idx = -1

        is_toggle = (open_marker == close_marker)
        open_len = len(open_marker)
        close_len = len(close_marker)

        while i < len(text):
            # Check for close marker first (if we are inside a block)
            # For toggle markers, this is the same as open, so we check depth
            if depth > 0 and text.startswith(close_marker, i):
                if is_toggle:
                    depth = 0 # Toggle off
                else:
                    depth -= 1

                i += close_len

                if depth == 0:
                    # Found end of outermost block
                    original_block = text[start_idx:i]
                    token = f"__BLOCK_{uuid.uuid4().hex[:8]}__"
                    self.replacements[token] = original_block
                    result_parts.append(token)
                    current_idx = i
                continue

            # Check for open marker
            if text.startswith(open_marker, i):
                if depth == 0:
                    # Start of a new outermost block
                    result_parts.append(text[current_idx:i])
                    start_idx = i

                if is_toggle:
                    if depth == 0: depth = 1 # Toggle on
                else:
                    depth += 1

                i += open_len
                continue

            i += 1

        # Append remaining text
        result_parts.append(text[current_idx:])

        # If we have unbalanced markers at the end, the string just stays as is
        # (effectively implicit closing or error caught later by parser)
        return "".join(result_parts)

    def unmask(self, text: str) -> str:
        """Recursively restore all tokens in the text."""
        if not text:
            return text

        # We loop until no more tokens are found to handle potential nesting
        # (though our current logic masks outermost, so one pass usually works)
        # However, line filtering might have joined tokens, so simple replace is safe.
        result = text
        while True:
            replaced_count = 0
            for token, original in self.replacements.items():
                if token in result:
                    result = result.replace(token, original)
                    replaced_count += 1
            if replaced_count == 0:
                break
        return result


class EmailDSLParser:
    """Parser for EmailDSL."""

    def __init__(self):
        self.parser = Lark(
            GRAMMAR,
            parser="lalr",
        )

    def parse(self, text: str) -> Document:
        """Parse EmailDSL text into AST.

        This method masks braces before parsing so the grammar only needs to
        recognize block tokens, not handle bracket matching logic.
        """
        masker = BlockMasker()

        # Mask all brace types
        text = masker.mask(text, "```", "```")
        text = masker.mask(text, "{{", "}}")
        text = masker.mask(text, "{", "}")

        # Parse with block tokens
        tree = self.parser.parse(text)

        # Transform and unmask at AST level
        transformer = EmailDSLTransformer(masker)
        return transformer.transform(tree)

    def parse_lines(self, text: str) -> Document:
        """Parse EmailDSL with stateless line-based filtering.

        Strategy:
        1. Mask hierarchy of blocks (Code -> Double Brace -> Single Brace).
           This hides body content inside safe tokens like __BLOCK_XYZ__.
        2. Filter lines. Since bodies are now single tokens on the definition line,
           we can simply check if the line starts with a DSL character.
           Noise lines (signatures, greetings) won't have tokens and won't start with chars.
        3. Parse with block tokens (grammar recognizes tokens, not braces).
        4. Unmask during AST transformation.
        """
        masker = BlockMasker()

        # 1. Hierarchy of Protection
        # Protect code blocks first (strongest)
        text = masker.mask(text, "```", "```")
        # Protect double braces next
        text = masker.mask(text, "{{", "}}")
        # Protect standard bodies last (weakest)
        text = masker.mask(text, "{", "}")

        # 2. Stateless Filter
        filtered_lines = []
        for line in text.split("\n"):
            stripped = line.lstrip()
            # If a line starts with a command char, keep it.
            # Because bodies are masked into tokens on these lines, we keep the bodies too.
            # Special chars: # (hashtag), : (attribute), / (item/vote), @ (email), ! (future use)
            if stripped and stripped[0] in "#:/@!":
                filtered_lines.append(line)

        filtered_text = "\n".join(filtered_lines)

        # 3. Parse with block tokens (no unmasking needed before parse)
        tree = self.parser.parse(filtered_text)

        # 4. Transform and unmask at AST level
        transformer = EmailDSLTransformer(masker)
        return transformer.transform(tree)

    def parse_full(self, text: str) -> Document:
        """Parse EmailDSL preserving prose for rendering.

        Unlike parse_lines(), this captures ALL content including non-DSL text,
        wrapping it in Prose nodes. Downstream consumers can filter as needed:
        - Evaluator: filters to DSL-only for processing votes/items
        - Renderer: uses all nodes to display complete document

        Strategy:
        1. Mask blocks to protect bodies
        2. Parse line-by-line, classifying as DSL or prose
        3. Return Document with interleaved Prose and DSL nodes
        """
        masker = BlockMasker()

        # Mask hierarchy of blocks
        text = masker.mask(text, "```", "```")
        text = masker.mask(text, "{{", "}}")
        text = masker.mask(text, "{", "}")

        statements = []
        prose_buffer = []

        for line in text.split("\n"):
            stripped = line.lstrip()

            # Check if line is DSL command
            if stripped and stripped[0] in "#:/@!":
                # Flush prose buffer first
                if prose_buffer:
                    prose_text = "\n".join(prose_buffer)
                    # Unmask prose text to restore original content
                    prose_text = masker.unmask(prose_text)
                    statements.append(Prose(text=prose_text))
                    prose_buffer = []

                # Parse DSL line
                try:
                    tree = self.parser.parse(line)
                    transformer = EmailDSLTransformer(masker)
                    doc = transformer.transform(tree)
                    statements.extend(doc.statements)
                except Exception:
                    # Parse failed, treat as prose
                    prose_buffer.append(line)
            else:
                # Not a DSL line, add to prose buffer
                prose_buffer.append(line)

        # Final flush
        if prose_buffer:
            prose_text = "\n".join(prose_buffer)
            prose_text = masker.unmask(prose_text)
            statements.append(Prose(text=prose_text))

        return Document(statements=statements)
