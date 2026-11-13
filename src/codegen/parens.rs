//! Removal of redundant parentheses from generated function source.
//!
//! The lowering parenthesises every composed expression (`(a + b)`,
//! `(x).wrapping_add(y)`) so that precedence never depends on where a value is
//! later spliced. At a few positions the wrapper is then redundant, and
//! `rustc`'s `unused_parens` early lint pays for each one by copying the rest of
//! the source file (`SourceMap::span_to_next_source`), which is quadratic on a
//! very large module (see `docs/f4-rustc-time-spike.md`). `#[allow]` does not
//! avoid that work, so the parentheses must not be emitted at all.
//!
//! Only positions where the parentheses can never change the parse are
//! rewritten, and only when the wrapped text has no top-level comma (so a tuple
//! stays a tuple):
//!
//! * a call/method argument or tuple/array element: `f((x), y)`, `f(a, (x))`;
//! * the right-hand side of a plain assignment or `let`: `x = (v);`;
//! * the tail of a block: `{ (v) }`, unless `v` starts like a block-like
//!   statement (`if`, `match`, ...), which would end the statement early and
//!   turn a following operator into a syntax error.
//!
//! Everything else is kept: a receiver (`(-2i32).rotate_left(n)` — a negative
//! literal binds looser than a method call), operands of operators and casts,
//! `return (..)`, conditions. The pass is lexical, so string and character
//! literals and comments are skipped; on any imbalance the input is returned
//! unchanged.

use std::borrow::Cow;

/// Remove the redundant parentheses described in the [module docs](self).
pub(crate) fn strip_redundant_parens(src: &str) -> Cow<'_, str> {
    match redundant_pairs(src.as_bytes()) {
        Some(mut drops) if !drops.is_empty() => {
            drops.sort_unstable();
            let bytes = src.as_bytes();
            let mut out = Vec::with_capacity(bytes.len().saturating_sub(drops.len()));
            let mut from = 0;
            for at in drops {
                out.extend_from_slice(bytes.get(from..at).unwrap_or_default());
                from = at.saturating_add(1);
            }
            out.extend_from_slice(bytes.get(from..).unwrap_or_default());
            // Only ASCII parentheses were removed from valid UTF-8, so this holds;
            // fall back to the input rather than panic if it somehow did not.
            String::from_utf8(out).map_or(Cow::Borrowed(src), Cow::Owned)
        }
        _ => Cow::Borrowed(src),
    }
}

/// Byte offsets of the `(` and `)` of every removable pair, or `None` if the
/// delimiters of `s` do not balance.
fn redundant_pairs(s: &[u8]) -> Option<Vec<usize>> {
    // Open delimiters: (offset, saw a comma directly inside).
    let mut stack: Vec<(usize, bool)> = Vec::new();
    let mut drops = Vec::new();
    let mut i = 0;
    while let Some(&c) = s.get(i) {
        match c {
            b'"' => i = skip_string(s, i)?,
            b'\'' => i = skip_char_or_lifetime(s, i),
            b'/' if s.get(i.saturating_add(1)) == Some(&b'/') => i = skip_line_comment(s, i),
            b'/' if s.get(i.saturating_add(1)) == Some(&b'*') => i = skip_block_comment(s, i)?,
            b'r' if is_raw_string_start(s, i) => i = skip_raw_string(s, i)?,
            b'(' | b'[' | b'{' => {
                stack.push((i, false));
                i = i.saturating_add(1);
            }
            b',' => {
                if let Some(top) = stack.last_mut() {
                    top.1 = true;
                }
                i = i.saturating_add(1);
            }
            b')' | b']' | b'}' => {
                let (open, comma) = stack.pop()?;
                let expected = match s.get(open)? {
                    b'(' => b')',
                    b'[' => b']',
                    _ => b'}',
                };
                if c != expected {
                    return None;
                }
                if c == b')' && !comma && is_removable(s, open, i) {
                    drops.push(open);
                    drops.push(i);
                }
                i = i.saturating_add(1);
            }
            _ => i = i.saturating_add(1),
        }
    }
    stack.is_empty().then_some(drops)
}

/// Whether the pair `(` at `open` .. `)` at `close` (no top-level comma) sits in
/// a position where it is redundant.
fn is_removable(s: &[u8], open: usize, close: usize) -> bool {
    if close <= open.saturating_add(1) {
        return false; // `()` is the unit value, not a grouping
    }
    let before = |n: usize| open.checked_sub(n).and_then(|at| s.get(at..open));
    let after = s.get(close.saturating_add(1)..).unwrap_or_default();
    let next = after.first();
    // Argument / element: directly after `(` or `, `, directly before `,` or `)`.
    if (before(1) == Some(b"(") || before(2) == Some(b", ")) && matches!(next, Some(b',' | b')')) {
        return true;
    }
    // Assigned value: ` = (..);` (not `==`, `<=`, `+=`: those have another byte before the `=`).
    if before(3) == Some(b" = ") && next == Some(&b';') {
        return true;
    }
    // Block tail: `{ (..) }` possibly across lines.
    let prev = s
        .get(..open)
        .and_then(|head| head.iter().rev().find(|b| !b.is_ascii_whitespace()));
    let tail = after.iter().find(|b| !b.is_ascii_whitespace());
    if prev == Some(&b'{') && tail == Some(&b'}') {
        let content = s.get(open.saturating_add(1)..close).unwrap_or_default();
        return !starts_block_like(content);
    }
    false
}

/// Whether `content`, used as a statement, would end at its first block: `if`,
/// `match`, `loop`, `while`, `for`, `unsafe`, a bare or labelled block.
fn starts_block_like(content: &[u8]) -> bool {
    // Look through nested wrapping parentheses: when they are dropped together
    // (`{ ((if ..)) }`), the innermost text becomes the statement.
    let content = content.trim_ascii_start();
    let content = content
        .iter()
        .position(|b| !b.is_ascii_whitespace() && *b != b'(')
        .and_then(|at| content.get(at..))
        .unwrap_or_default();
    if matches!(content.first(), Some(b'{' | b'\'')) {
        return true;
    }
    let word_len = content
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
        .count();
    matches!(
        content.get(..word_len),
        Some(b"if" | b"match" | b"loop" | b"while" | b"for" | b"unsafe")
    )
}

/// Offset just past the string literal that opens at `start`.
fn skip_string(s: &[u8], start: usize) -> Option<usize> {
    let mut i = start.saturating_add(1);
    loop {
        match s.get(i)? {
            b'\\' => i = i.saturating_add(2),
            b'"' => return Some(i.saturating_add(1)),
            _ => i = i.saturating_add(1),
        }
    }
}

/// `r"`, `r#"`, `br"`...: an `r` that starts a token (not the end of an identifier).
fn is_raw_string_start(s: &[u8], at: usize) -> bool {
    let is_ident = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_');
    let at_before = |n: usize| at.checked_sub(n).and_then(|p| s.get(p));
    let token_start = if at_before(1) == Some(&b'b') {
        !is_ident(at_before(2))
    } else {
        !is_ident(at_before(1))
    };
    if !token_start {
        return false;
    }
    let hashes = s
        .get(at.saturating_add(1)..)
        .unwrap_or_default()
        .iter()
        .take_while(|b| **b == b'#')
        .count();
    s.get(at.saturating_add(1).saturating_add(hashes)) == Some(&b'"')
}

fn skip_raw_string(s: &[u8], start: usize) -> Option<usize> {
    let rest = s.get(start.saturating_add(1)..)?;
    let hashes = rest.iter().take_while(|b| **b == b'#').count();
    let body = start.saturating_add(hashes).saturating_add(2);
    let mut closer = vec![b'"'];
    closer.resize(hashes.saturating_add(1), b'#');
    let tail = s.get(body..)?;
    let at = tail.windows(closer.len()).position(|w| w == closer)?;
    Some(body.saturating_add(at).saturating_add(closer.len()))
}

/// A character literal (`'('`, `'\''`, `'é'`) is skipped whole; a lifetime or
/// loop label (`'a`, `'l0:`) has no closing quote, so only the quote is skipped.
fn skip_char_or_lifetime(s: &[u8], start: usize) -> usize {
    let first = start.saturating_add(1);
    if s.get(first) == Some(&b'\\') {
        let close = s
            .get(first.saturating_add(2)..)
            .and_then(|rest| rest.iter().position(|b| *b == b'\''));
        return close.map_or(first, |c| {
            first.saturating_add(2).saturating_add(c).saturating_add(1)
        });
    }
    // One scalar value (up to 4 UTF-8 bytes) then the closing quote.
    for len in 1..=4usize {
        if s.get(first.saturating_add(len)) == Some(&b'\'')
            && s.get(first).is_some_and(|b| *b != b'\'')
        {
            return first.saturating_add(len).saturating_add(1);
        }
    }
    first
}

fn skip_line_comment(s: &[u8], start: usize) -> usize {
    s.get(start..)
        .and_then(|rest| rest.iter().position(|b| *b == b'\n'))
        .map_or(s.len(), |n| start.saturating_add(n))
}

/// Block comments nest in Rust.
fn skip_block_comment(s: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = start;
    while i < s.len() {
        match (s.get(i), s.get(i.saturating_add(1))) {
            (Some(b'/'), Some(b'*')) => {
                depth = depth.saturating_add(1);
                i = i.saturating_add(2);
            }
            (Some(b'*'), Some(b'/')) => {
                depth = depth.saturating_sub(1);
                i = i.saturating_add(2);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => i = i.saturating_add(1),
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::strip_redundant_parens as strip;

    #[test]
    fn strips_call_and_method_arguments() {
        assert_eq!(strip("self.w32((a + b), v);"), "self.w32(a + b, v);");
        assert_eq!(strip("f(a, (b + c));"), "f(a, b + c);");
        assert_eq!(strip("f(((a + b)));"), "f(a + b);");
        assert_eq!(strip("i32::from((x == 0));"), "i32::from(x == 0);");
    }

    #[test]
    fn strips_assigned_and_let_values() {
        assert_eq!(strip("let v1: i32 = (a + b);"), "let v1: i32 = a + b;");
        assert_eq!(
            strip("x = (a).wrapping_add(b);"),
            "x = (a).wrapping_add(b);"
        );
        assert_eq!(
            strip("x = ((a).wrapping_add(b));"),
            "x = (a).wrapping_add(b);"
        );
    }

    #[test]
    fn strips_block_tails() {
        assert_eq!(
            strip("if c { (a + b) } else { (c) }"),
            "if c { a + b } else { c }"
        );
        assert_eq!(strip("{\n    (a + b)\n}"), "{\n    a + b\n}");
    }

    #[test]
    fn keeps_block_like_tails() {
        // `if x {1} else {2} + 1` as a statement would end after the `if`.
        let src = "{ (if x { 1 } else { 2 } + 1) }";
        assert_eq!(strip(src), src);
        let src = "{ (match x { _ => 1 } + 1) }";
        assert_eq!(strip(src), src);
        let src = "{ ({ 1 } + 1) }";
        assert_eq!(strip(src), src);
        // Doubled parentheses: dropping both would expose the block-like head, so
        // one pair must stay.
        assert_eq!(
            strip("{ ((if x { 1 } else { 2 } + 1)) }"),
            "{ (if x { 1 } else { 2 } + 1) }"
        );
    }

    #[test]
    fn keeps_receivers_and_operands() {
        // Unary minus binds looser than a method call: this paren is load-bearing.
        let src = "f((-2i32).rotate_left(n as u32));";
        assert_eq!(strip(src), src);
        let src = "((a) + (b))";
        assert_eq!(strip(src), src);
        let src = "f((a as u32) < (b as u32));";
        assert_eq!(strip(src), src);
        let src = "f((a) as u64, (b)..(c));";
        assert_eq!(strip(src), src);
        let src = "return (a + b);";
        assert_eq!(strip(src), src);
        let src = "if (a) == (b) { }";
        assert_eq!(strip(src), src);
        let src = "x += (a);\ny == (b);\nz <= (c);";
        assert_eq!(strip(src), src);
    }

    #[test]
    fn keeps_tuples_unit_and_single_element_tuples() {
        let src = "f((a, b), c);";
        assert_eq!(strip(src), src);
        let src = "let t = (a, b);";
        assert_eq!(strip(src), src);
        let src = "f(());";
        assert_eq!(strip(src), src);
        // `((a),)` -> `(a,)`: still a one-element tuple.
        assert_eq!(strip("let t = ((a),);"), "let t = (a,);");
    }

    #[test]
    fn ignores_delimiters_in_literals_and_comments() {
        let src = "f(b\"(\\\"(\", ')');\nlet c = '\\'';\n// (x)\nf((a));";
        assert_eq!(
            strip(src),
            "f(b\"(\\\"(\", ')');\nlet c = '\\'';\n// (x)\nf(a);"
        );
        let src = "'l0: loop { f(('a')); } /* ( */ g(('(')); r#\"(\"# ";
        assert_eq!(
            strip(src),
            "'l0: loop { f('a'); } /* ( */ g('('); r#\"(\"# "
        );
    }

    #[test]
    fn unbalanced_input_is_left_alone() {
        let src = "f((a);";
        assert_eq!(strip(src), src);
        let src = "f((a)));";
        assert_eq!(strip(src), src);
    }
}
