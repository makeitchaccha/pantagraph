//! Query DSL for combining tag references with AND, OR, and NOT.
//!
//! Grammar (highest to lowest precedence: `!`, then `*` and implicit AND,
//! then `+`):
//!
//! ```text
//! expr   := or
//! or     := and ("+" and)*
//! and    := unary (("*" unary) | unary)*
//! unary  := "!" unary | primary
//! primary := TAG | META | "(" expr ")"
//! META    := "meta:" key ["=" value]
//! ```
//!
//! Semantics are set operations over entries: `A * B` is intersection,
//! `A + B` is union, and `!A` is the complement over all entries.
//! Whitespace between adjacent terms is an implicit AND, so `A B` and
//! `A * B` are equivalent.
//!
//! `meta:` is a reserved prefix: `meta:key=value` matches entries whose
//! `key` meta attribute equals `value` exactly, and `meta:key` matches
//! entries that have the `key` attribute at any value. A key with no
//! matching entries matches nothing, so `!meta:key` matches everything.
//!
//! Parsing only builds the [`Query`] AST; tag references stay verbatim
//! strings and are resolved to [`TagId`]s in a separate step. Meta
//! predicates need no resolution and pass through unchanged.

use std::fmt;

use crate::domain::TagId;

/// A parsed query over tag references.
///
/// The default form (`Query`, i.e. `Query<String>`) holds raw tag
/// reference strings as produced by the parser — the same references
/// accepted by `-t`. After resolution it becomes `Query<TagId>`, also
/// available as [`ResolvedQuery`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query<T = String> {
    /// A single tag reference.
    Tag(T),
    /// An entry meta attribute predicate: `meta:key` matches entries
    /// holding `key` at any value; `meta:key=value` matches entries whose
    /// `key` equals `value` exactly. `T` is unused — meta keys resolve to
    /// themselves.
    Meta {
        /// Meta attribute key (free-form, case-sensitive).
        key: String,
        /// Expected value, or `None` to test presence only.
        value: Option<String>,
    },
    /// All operands match (intersection).
    And(Vec<Query<T>>),
    /// At least one operand matches (union).
    Or(Vec<Query<T>>),
    /// Entries not matched by the inner query (complement).
    Not(Box<Query<T>>),
}

/// A [`Query`] whose tag references have been resolved to [`TagId`]s.
pub type ResolvedQuery = Query<TagId>;

/// Error produced by [`parse`], carrying the byte offset of the problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryParseError {
    /// Byte offset into the input where the error was detected.
    pub position: usize,
    /// Description of what went wrong.
    pub message: String,
}

impl QueryParseError {
    fn new(position: usize, message: impl Into<String>) -> Self {
        Self {
            position,
            message: message.into(),
        }
    }
}

impl fmt::Display for QueryParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid query at byte {}: {}",
            self.position, self.message
        )
    }
}

impl std::error::Error for QueryParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Tag(String),
    Plus,
    Star,
    Bang,
    LParen,
    RParen,
}

impl TokenKind {
    fn describe(&self) -> String {
        match self {
            TokenKind::Tag(s) => format!("`{s}`"),
            TokenKind::Plus => "`+`".to_string(),
            TokenKind::Star => "`*`".to_string(),
            TokenKind::Bang => "`!`".to_string(),
            TokenKind::LParen => "`(`".to_string(),
            TokenKind::RParen => "`)`".to_string(),
        }
    }
}

struct Token {
    kind: TokenKind,
    position: usize,
}

fn is_reserved(c: char) -> bool {
    matches!(c, '+' | '*' | '!' | '(' | ')')
}

/// Reserved query prefix introducing a meta attribute predicate.
///
/// `:` stays unreserved for the tokenizer so `meta:key=value` lexes as a
/// single [`TokenKind::Tag`]; the prefix is only interpreted in
/// [`Parser::parse_primary`].
const META_PREFIX: &str = "meta:";

/// Parse the body of a `meta:` term (everything after the prefix).
///
/// `key` matches any value, `key=value` matches that exact value. An
/// empty key is rejected; an empty value (`key=`) is an exact match
/// against the empty string.
fn parse_meta(rest: &str, token_position: usize) -> Result<Query, QueryParseError> {
    let (key, value) = match rest.split_once('=') {
        Some((key, value)) => (key, Some(value.to_string())),
        None => (rest, None),
    };
    if key.is_empty() {
        return Err(QueryParseError::new(
            token_position,
            "empty meta key after `meta:`",
        ));
    }
    Ok(Query::Meta {
        key: key.to_string(),
        value,
    })
}

fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < input.len() {
        let c = input[i..].chars().next().expect("i is on a char boundary");
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let position = i;
        let kind = if is_reserved(c) {
            i += c.len_utf8();
            match c {
                '+' => TokenKind::Plus,
                '*' => TokenKind::Star,
                '!' => TokenKind::Bang,
                '(' => TokenKind::LParen,
                _ => TokenKind::RParen,
            }
        } else {
            let start = i;
            while i < input.len() {
                let c = input[i..].chars().next().expect("i is on a char boundary");
                if c.is_whitespace() || is_reserved(c) {
                    break;
                }
                i += c.len_utf8();
            }
            TokenKind::Tag(input[start..i].to_string())
        };
        tokens.push(Token { kind, position });
    }
    tokens
}

struct Parser {
    tokens: std::iter::Peekable<std::vec::IntoIter<Token>>,
    input_len: usize,
}

impl Parser {
    fn eof(&self) -> QueryParseError {
        QueryParseError::new(self.input_len, "unexpected end of input")
    }

    fn parse_expr(&mut self) -> Result<Query, QueryParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Query, QueryParseError> {
        let mut operands = vec![self.parse_and()?];
        while matches!(self.tokens.peek(), Some(t) if t.kind == TokenKind::Plus) {
            self.tokens.next();
            operands.push(self.parse_and()?);
        }
        Ok(if operands.len() == 1 {
            operands.pop().expect("one operand")
        } else {
            Query::Or(operands)
        })
    }

    fn parse_and(&mut self) -> Result<Query, QueryParseError> {
        let mut operands = vec![self.parse_unary()?];
        loop {
            if matches!(self.tokens.peek(), Some(t) if t.kind == TokenKind::Star) {
                self.tokens.next();
                operands.push(self.parse_unary()?);
            } else if matches!(
                self.tokens.peek(),
                Some(t)
                    if matches!(
                        t.kind,
                        TokenKind::Tag(_) | TokenKind::Bang | TokenKind::LParen
                    )
            ) {
                operands.push(self.parse_unary()?);
            } else {
                break;
            }
        }
        Ok(if operands.len() == 1 {
            operands.pop().expect("one operand")
        } else {
            Query::And(operands)
        })
    }

    fn parse_unary(&mut self) -> Result<Query, QueryParseError> {
        if matches!(self.tokens.peek(), Some(t) if t.kind == TokenKind::Bang) {
            self.tokens.next();
            return Ok(Query::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Query, QueryParseError> {
        let token = match self.tokens.next() {
            Some(token) => token,
            None => return Err(self.eof()),
        };
        match token.kind {
            TokenKind::Tag(tag) => match tag.strip_prefix(META_PREFIX) {
                Some(rest) => parse_meta(rest, token.position),
                None => Ok(Query::Tag(tag)),
            },
            TokenKind::LParen => {
                let inner = self.parse_expr()?;
                match self.tokens.next() {
                    Some(t) if t.kind == TokenKind::RParen => Ok(inner),
                    Some(t) => Err(QueryParseError::new(
                        t.position,
                        format!("expected `)`, found {}", t.kind.describe()),
                    )),
                    None => Err(QueryParseError::new(self.input_len, "missing closing `)`")),
                }
            }
            kind => Err(QueryParseError::new(
                token.position,
                format!("expected expression, found {}", kind.describe()),
            )),
        }
    }
}

/// Parse a query string into a [`Query`] AST.
///
/// # Errors
///
/// Returns [`QueryParseError`] with the offending byte offset for empty
/// or malformed input (unexpected end, missing `)`, misplaced operators).
pub fn parse(input: &str) -> Result<Query, QueryParseError> {
    let mut parser = Parser {
        tokens: tokenize(input).into_iter().peekable(),
        input_len: input.len(),
    };
    let query = parser.parse_expr()?;
    if let Some(token) = parser.tokens.peek() {
        return Err(QueryParseError::new(
            token.position,
            format!("unexpected token {}", token.kind.describe()),
        ));
    }
    Ok(query)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tag(name: &str) -> Query {
        Query::Tag(name.to_string())
    }

    fn and(operands: Vec<Query>) -> Query {
        Query::And(operands)
    }

    fn or(operands: Vec<Query>) -> Query {
        Query::Or(operands)
    }

    fn not(inner: Query) -> Query {
        Query::Not(Box::new(inner))
    }

    #[test]
    fn test_parse_single_tag() {
        assert_eq!(parse("A").unwrap(), tag("A"));
    }

    #[test]
    fn test_parse_implicit_and() {
        assert_eq!(parse("A B").unwrap(), and(vec![tag("A"), tag("B")]));
    }

    #[test]
    fn test_parse_explicit_and() {
        assert_eq!(parse("A * B").unwrap(), and(vec![tag("A"), tag("B")]));
    }

    #[test]
    fn test_parse_implicit_and_equals_explicit_and() {
        assert_eq!(
            parse("A B C").unwrap(),
            and(vec![tag("A"), tag("B"), tag("C")])
        );
        assert_eq!(parse("A B C").unwrap(), parse("A * B * C").unwrap());
    }

    #[test]
    fn test_parse_or() {
        assert_eq!(parse("A + B").unwrap(), or(vec![tag("A"), tag("B")]));
    }

    #[test]
    fn test_parse_not() {
        assert_eq!(parse("!A").unwrap(), not(tag("A")));
    }

    #[test]
    fn test_parse_precedence_and_binds_tighter_than_or() {
        // `A + B C` means `A OR (B AND C)`.
        assert_eq!(
            parse("A + B C").unwrap(),
            or(vec![tag("A"), and(vec![tag("B"), tag("C")])])
        );
        assert_eq!(
            parse("A + B * C").unwrap(),
            or(vec![tag("A"), and(vec![tag("B"), tag("C")])])
        );
        assert_eq!(
            parse("A * B + C").unwrap(),
            or(vec![and(vec![tag("A"), tag("B")]), tag("C")])
        );
    }

    #[test]
    fn test_parse_precedence_not_binds_tighter_than_and() {
        // `A * !B` means `A AND (NOT B)`.
        assert_eq!(parse("A * !B").unwrap(), and(vec![tag("A"), not(tag("B"))]));
        assert_eq!(parse("!!A").unwrap(), not(not(tag("A"))));
    }

    #[test]
    fn test_parse_parentheses() {
        assert_eq!(
            parse("(A + B) * !C").unwrap(),
            and(vec![or(vec![tag("A"), tag("B")]), not(tag("C"))])
        );
        assert_eq!(parse("(A)").unwrap(), tag("A"));
        assert_eq!(
            parse("A (B + C)").unwrap(),
            and(vec![tag("A"), or(vec![tag("B"), tag("C")])])
        );
    }

    #[test]
    fn test_parse_hyphenated_tags_are_single_terms() {
        assert_eq!(
            parse("clear-amber quiet-bright").unwrap(),
            and(vec![tag("clear-amber"), tag("quiet-bright")])
        );
    }

    #[test]
    fn test_parse_operators_without_surrounding_spaces() {
        assert_eq!(parse("A+B").unwrap(), or(vec![tag("A"), tag("B")]));
        assert_eq!(parse("!(A+B)").unwrap(), not(or(vec![tag("A"), tag("B")])));
    }

    #[test]
    fn test_parse_error_unexpected_end_of_input() {
        assert_eq!(
            parse(""),
            Err(QueryParseError::new(0, "unexpected end of input"))
        );
        assert_eq!(
            parse("A +"),
            Err(QueryParseError::new(3, "unexpected end of input"))
        );
        assert_eq!(
            parse("A *"),
            Err(QueryParseError::new(3, "unexpected end of input"))
        );
        assert_eq!(
            parse("("),
            Err(QueryParseError::new(1, "unexpected end of input"))
        );
        assert_eq!(
            parse("!"),
            Err(QueryParseError::new(1, "unexpected end of input"))
        );
    }

    #[test]
    fn test_parse_error_missing_closing_paren() {
        assert_eq!(
            parse("(A + B"),
            Err(QueryParseError::new(6, "missing closing `)`"))
        );
    }

    #[test]
    fn test_parse_error_empty_group() {
        assert_eq!(
            parse("()"),
            Err(QueryParseError::new(1, "expected expression, found `)`"))
        );
        assert_eq!(
            parse("(A +)"),
            Err(QueryParseError::new(4, "expected expression, found `)`"))
        );
    }

    #[test]
    fn test_parse_error_misplaced_operators() {
        // Consecutive binary operators.
        assert_eq!(
            parse("A * * B"),
            Err(QueryParseError::new(4, "expected expression, found `*`"))
        );
        assert_eq!(
            parse("A + + B"),
            Err(QueryParseError::new(4, "expected expression, found `+`"))
        );
        // Leading binary operator.
        assert_eq!(
            parse("* A"),
            Err(QueryParseError::new(0, "expected expression, found `*`"))
        );
        // Stray closing paren.
        assert_eq!(
            parse("A )"),
            Err(QueryParseError::new(2, "unexpected token `)`"))
        );
        assert_eq!(
            parse("(A) B)"),
            Err(QueryParseError::new(5, "unexpected token `)`"))
        );
    }

    #[test]
    fn test_parse_meta_key_only() {
        assert_eq!(
            parse("meta:archiver").unwrap(),
            Query::Meta {
                key: "archiver".to_string(),
                value: None,
            }
        );
    }

    #[test]
    fn test_parse_meta_key_value() {
        assert_eq!(
            parse("meta:archiver=yt-dlp").unwrap(),
            Query::Meta {
                key: "archiver".to_string(),
                value: Some("yt-dlp".to_string()),
            }
        );
    }

    #[test]
    fn test_parse_meta_with_and_not() {
        assert_eq!(
            parse("A * meta:archived=true").unwrap(),
            Query::And(vec![
                Query::Tag("A".to_string()),
                Query::Meta {
                    key: "archived".to_string(),
                    value: Some("true".to_string()),
                },
            ])
        );
        assert_eq!(
            parse("!meta:archived").unwrap(),
            Query::Not(Box::new(Query::Meta {
                key: "archived".to_string(),
                value: None,
            }))
        );
    }

    #[test]
    fn test_parse_meta_empty_key_error() {
        assert_eq!(
            parse("meta:").unwrap_err(),
            QueryParseError::new(0, "empty meta key after `meta:`")
        );
    }

    #[test]
    fn test_query_parse_error_display() {
        let err = parse("A +").unwrap_err();
        assert_eq!(
            err.to_string(),
            "invalid query at byte 3: unexpected end of input"
        );
    }
}
