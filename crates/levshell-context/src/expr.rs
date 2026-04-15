//! Relevance rule expression DSL.
//!
//! A tiny predicate language over [`SignalContext`] values. Grammar:
//!
//! ```text
//! Expr    := Or
//! Or      := And ("or" And)*
//! And     := Unary ("and" Unary)*
//! Unary   := "not" Unary | Atom
//! Atom    := "(" Expr ")" | Comparison
//! Comparison := Term (CmpOp Term)?      // bare term = truthy check
//! CmpOp   := "==" | "!=" | "<" | "<=" | ">" | ">=" | "contains"
//! Term    := Number | String | Bool | SignalRef
//! SignalRef := Ident ("." Ident)*
//! ```
//!
//! Examples:
//!
//! ```text
//! focused.app_id == "firefox"
//! workspace.tags contains "research"
//! battery.percent < 30 and power.on_battery
//! not (focused.app_id == "obs")
//! ```
//!
//! The parser is a hand-rolled recursive descent over a token list. It
//! produces an [`Expression`] AST; [`evaluate`] walks the AST against a
//! [`SignalContext`] and returns a `bool` (plus error on type mismatch).

use crate::error::{ContextError, Result};
use crate::signals::{SignalContext, SignalValue};

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    And(Box<Expression>, Box<Expression>),
    Or(Box<Expression>, Box<Expression>),
    Not(Box<Expression>),
    Compare {
        lhs: Term,
        op: CompareOp,
        rhs: Term,
    },
    /// A bare term in boolean position (e.g. `power.on_battery` without an
    /// explicit `== true`). Evaluates truthy for `Bool(true)`, falsy for
    /// `Bool(false)` or a missing signal, and is an eval error otherwise.
    Truthy(Term),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Signal(String),
    Literal(Literal),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Bool(bool),
    Number(f64),
    String(String),
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Contains,
}

impl CompareOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            CompareOp::Eq => "==",
            CompareOp::NotEq => "!=",
            CompareOp::Lt => "<",
            CompareOp::LtEq => "<=",
            CompareOp::Gt => ">",
            CompareOp::GtEq => ">=",
            CompareOp::Contains => "contains",
        }
    }
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Number(f64),
    String(String),
    Bool(bool),
    Dot,
    LParen,
    RParen,
    And,
    Or,
    Not,
    Contains,
    Op(CompareOp),
}

#[derive(Debug)]
struct SpannedToken {
    token: Token,
    position: usize,
}

fn tokenize(src: &str) -> Result<Vec<SpannedToken>> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        match c {
            b'(' => {
                out.push(SpannedToken {
                    token: Token::LParen,
                    position: start,
                });
                i += 1;
            }
            b')' => {
                out.push(SpannedToken {
                    token: Token::RParen,
                    position: start,
                });
                i += 1;
            }
            b'.' => {
                out.push(SpannedToken {
                    token: Token::Dot,
                    position: start,
                });
                i += 1;
            }
            b'=' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(SpannedToken {
                    token: Token::Op(CompareOp::Eq),
                    position: start,
                });
                i += 2;
            }
            b'!' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(SpannedToken {
                    token: Token::Op(CompareOp::NotEq),
                    position: start,
                });
                i += 2;
            }
            b'<' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(SpannedToken {
                    token: Token::Op(CompareOp::LtEq),
                    position: start,
                });
                i += 2;
            }
            b'<' => {
                out.push(SpannedToken {
                    token: Token::Op(CompareOp::Lt),
                    position: start,
                });
                i += 1;
            }
            b'>' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(SpannedToken {
                    token: Token::Op(CompareOp::GtEq),
                    position: start,
                });
                i += 2;
            }
            b'>' => {
                out.push(SpannedToken {
                    token: Token::Op(CompareOp::Gt),
                    position: start,
                });
                i += 1;
            }
            b'"' => {
                i += 1;
                let content_start = i;
                let mut buf = String::new();
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' && i + 1 < bytes.len() {
                        match bytes[i + 1] {
                            b'"' => buf.push('"'),
                            b'\\' => buf.push('\\'),
                            b'n' => buf.push('\n'),
                            b't' => buf.push('\t'),
                            other => buf.push(other as char),
                        }
                        i += 2;
                    } else {
                        buf.push(bytes[i] as char);
                        i += 1;
                    }
                }
                if i >= bytes.len() {
                    return Err(ContextError::parse(
                        "unterminated string literal",
                        content_start - 1,
                    ));
                }
                i += 1; // consume closing quote
                out.push(SpannedToken {
                    token: Token::String(buf),
                    position: start,
                });
            }
            b'0'..=b'9' | b'-' => {
                // A leading `-` is only a sign if it's immediately followed
                // by a digit; otherwise it's not something we recognize.
                let lead_dash = bytes[i] == b'-';
                if lead_dash && !matches!(bytes.get(i + 1), Some(d) if d.is_ascii_digit()) {
                    return Err(ContextError::parse(
                        format!("unexpected character `{}`", c as char),
                        start,
                    ));
                }
                let mut end = i;
                if lead_dash {
                    end += 1;
                }
                while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
                    end += 1;
                }
                let lexeme = &src[i..end];
                let n: f64 = lexeme
                    .parse()
                    .map_err(|_| ContextError::parse(format!("invalid number `{lexeme}`"), start))?;
                out.push(SpannedToken {
                    token: Token::Number(n),
                    position: start,
                });
                i = end;
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                let mut end = i;
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
                let lexeme = &src[i..end];
                let tok = match lexeme {
                    "and" => Token::And,
                    "or" => Token::Or,
                    "not" => Token::Not,
                    "contains" => Token::Contains,
                    "true" => Token::Bool(true),
                    "false" => Token::Bool(false),
                    other => Token::Ident(other.to_owned()),
                };
                out.push(SpannedToken {
                    token: tok,
                    position: start,
                });
                i = end;
            }
            _ => {
                return Err(ContextError::parse(
                    format!("unexpected character `{}`", c as char),
                    start,
                ));
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

pub fn parse_expression(src: &str) -> Result<Expression> {
    let tokens = tokenize(src)?;
    let mut p = Parser { tokens, pos: 0 };
    let expr = p.parse_expr()?;
    if p.pos < p.tokens.len() {
        return Err(ContextError::parse(
            format!("unexpected token `{:?}` after expression", p.tokens[p.pos].token),
            p.tokens[p.pos].position,
        ));
    }
    Ok(expr)
}

struct Parser {
    tokens: Vec<SpannedToken>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|t| &t.token)
    }

    fn peek_position(&self) -> usize {
        self.tokens
            .get(self.pos)
            .map(|t| t.position)
            .unwrap_or_else(|| self.tokens.last().map(|t| t.position + 1).unwrap_or(0))
    }

    fn advance(&mut self) -> Option<Token> {
        let tok = self.tokens.get(self.pos).map(|t| t.token.clone())?;
        self.pos += 1;
        Some(tok)
    }

    fn parse_expr(&mut self) -> Result<Expression> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expression> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.advance();
            let rhs = self.parse_and()?;
            lhs = Expression::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expression> {
        let mut lhs = self.parse_unary()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.advance();
            let rhs = self.parse_unary()?;
            lhs = Expression::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Expression> {
        if matches!(self.peek(), Some(Token::Not)) {
            self.advance();
            let inner = self.parse_unary()?;
            return Ok(Expression::Not(Box::new(inner)));
        }
        self.parse_atom()
    }

    fn parse_atom(&mut self) -> Result<Expression> {
        if matches!(self.peek(), Some(Token::LParen)) {
            self.advance();
            let inner = self.parse_expr()?;
            match self.advance() {
                Some(Token::RParen) => Ok(inner),
                _ => Err(ContextError::parse(
                    "expected `)`",
                    self.peek_position(),
                )),
            }
        } else {
            self.parse_comparison()
        }
    }

    fn parse_comparison(&mut self) -> Result<Expression> {
        let lhs_pos = self.peek_position();
        let lhs = self.parse_term()?;
        let op = match self.peek() {
            Some(Token::Op(op)) => Some(*op),
            Some(Token::Contains) => Some(CompareOp::Contains),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let rhs = self.parse_term()?;
            return Ok(Expression::Compare { lhs, op, rhs });
        }
        match lhs {
            Term::Signal(_) | Term::Literal(Literal::Bool(_)) => Ok(Expression::Truthy(lhs)),
            _ => Err(ContextError::parse(
                "non-boolean term requires a comparison operator",
                lhs_pos,
            )),
        }
    }

    fn parse_term(&mut self) -> Result<Term> {
        let pos = self.peek_position();
        match self.advance() {
            Some(Token::Number(n)) => Ok(Term::Literal(Literal::Number(n))),
            Some(Token::String(s)) => Ok(Term::Literal(Literal::String(s))),
            Some(Token::Bool(b)) => Ok(Term::Literal(Literal::Bool(b))),
            Some(Token::Ident(first)) => {
                let mut path = first;
                while matches!(self.peek(), Some(Token::Dot)) {
                    self.advance();
                    match self.advance() {
                        Some(Token::Ident(next)) => {
                            path.push('.');
                            path.push_str(&next);
                        }
                        _ => {
                            return Err(ContextError::parse(
                                "expected identifier after `.`",
                                self.peek_position(),
                            ));
                        }
                    }
                }
                Ok(Term::Signal(path))
            }
            Some(other) => Err(ContextError::parse(
                format!("unexpected token `{other:?}` in term"),
                pos,
            )),
            None => Err(ContextError::parse("unexpected end of input", pos)),
        }
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

pub fn evaluate(expr: &Expression, ctx: &SignalContext) -> Result<bool> {
    match expr {
        Expression::And(a, b) => Ok(evaluate(a, ctx)? && evaluate(b, ctx)?),
        Expression::Or(a, b) => Ok(evaluate(a, ctx)? || evaluate(b, ctx)?),
        Expression::Not(inner) => Ok(!evaluate(inner, ctx)?),
        Expression::Truthy(term) => eval_truthy(term, ctx),
        Expression::Compare { lhs, op, rhs } => eval_compare(*op, lhs, rhs, ctx),
    }
}

fn eval_truthy(term: &Term, ctx: &SignalContext) -> Result<bool> {
    match term {
        Term::Literal(Literal::Bool(b)) => Ok(*b),
        Term::Literal(other) => Err(ContextError::eval(format!(
            "non-boolean literal `{other:?}` in truthy position"
        ))),
        Term::Signal(name) => match ctx.get(name) {
            Some(SignalValue::Bool(b)) => Ok(*b),
            Some(other) => Err(ContextError::eval(format!(
                "signal `{name}` has type `{}`, expected bool in truthy position",
                other.type_name()
            ))),
            None => Ok(false),
        },
    }
}

fn eval_compare(
    op: CompareOp,
    lhs: &Term,
    rhs: &Term,
    ctx: &SignalContext,
) -> Result<bool> {
    let lhs_val = resolve_term(lhs, ctx);
    let rhs_val = resolve_term(rhs, ctx);

    // Missing signals collapse comparisons to `false` so a rule doesn't fire
    // when the data it needs isn't there yet.
    let (lhs_val, rhs_val) = match (lhs_val, rhs_val) {
        (Some(l), Some(r)) => (l, r),
        _ => return Ok(false),
    };

    match op {
        CompareOp::Eq => Ok(signal_eq(&lhs_val, &rhs_val)),
        CompareOp::NotEq => Ok(!signal_eq(&lhs_val, &rhs_val)),
        CompareOp::Lt | CompareOp::LtEq | CompareOp::Gt | CompareOp::GtEq => {
            let a = lhs_val.as_number().ok_or_else(|| {
                ContextError::eval(format!(
                    "cannot compare with `{}`: left side is `{}`, expected number",
                    op.as_str(),
                    lhs_val.type_name()
                ))
            })?;
            let b = rhs_val.as_number().ok_or_else(|| {
                ContextError::eval(format!(
                    "cannot compare with `{}`: right side is `{}`, expected number",
                    op.as_str(),
                    rhs_val.type_name()
                ))
            })?;
            Ok(match op {
                CompareOp::Lt => a < b,
                CompareOp::LtEq => a <= b,
                CompareOp::Gt => a > b,
                CompareOp::GtEq => a >= b,
                _ => unreachable!(),
            })
        }
        CompareOp::Contains => eval_contains(&lhs_val, &rhs_val),
    }
}

fn signal_eq(a: &SignalValue, b: &SignalValue) -> bool {
    match (a, b) {
        (SignalValue::Bool(x), SignalValue::Bool(y)) => x == y,
        (SignalValue::Number(x), SignalValue::Number(y)) => x == y,
        (SignalValue::String(x), SignalValue::String(y)) => x == y,
        (SignalValue::StringList(x), SignalValue::StringList(y)) => x == y,
        _ => false,
    }
}

fn eval_contains(haystack: &SignalValue, needle: &SignalValue) -> Result<bool> {
    match (haystack, needle) {
        (SignalValue::StringList(list), SignalValue::String(s)) => {
            Ok(list.iter().any(|item| item == s))
        }
        (SignalValue::String(text), SignalValue::String(s)) => Ok(text.contains(s.as_str())),
        (other_l, other_r) => Err(ContextError::eval(format!(
            "`contains` requires (string_list, string) or (string, string); got ({}, {})",
            other_l.type_name(),
            other_r.type_name()
        ))),
    }
}

fn resolve_term(term: &Term, ctx: &SignalContext) -> Option<SignalValue> {
    match term {
        Term::Literal(Literal::Bool(b)) => Some(SignalValue::Bool(*b)),
        Term::Literal(Literal::Number(n)) => Some(SignalValue::Number(*n)),
        Term::Literal(Literal::String(s)) => Some(SignalValue::String(s.clone())),
        Term::Signal(name) => ctx.get(name).cloned(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_basic() -> SignalContext {
        SignalContext::new()
            .with("focused.app_id", "firefox")
            .with("battery.percent", 42.0_f64)
            .with("power.on_battery", true)
            .with(
                "workspace.tags",
                vec!["research".to_string(), "reading".to_string()],
            )
    }

    #[test]
    fn parses_eq_on_string_signal() {
        let e = parse_expression(r#"focused.app_id == "firefox""#).unwrap();
        assert!(evaluate(&e, &ctx_basic()).unwrap());
    }

    #[test]
    fn parses_neq() {
        let e = parse_expression(r#"focused.app_id != "obs""#).unwrap();
        assert!(evaluate(&e, &ctx_basic()).unwrap());
    }

    #[test]
    fn parses_numeric_comparison() {
        let e = parse_expression("battery.percent < 50").unwrap();
        assert!(evaluate(&e, &ctx_basic()).unwrap());
        let e = parse_expression("battery.percent > 100").unwrap();
        assert!(!evaluate(&e, &ctx_basic()).unwrap());
    }

    #[test]
    fn parses_contains_on_string_list() {
        let e = parse_expression(r#"workspace.tags contains "research""#).unwrap();
        assert!(evaluate(&e, &ctx_basic()).unwrap());
        let e = parse_expression(r#"workspace.tags contains "writing""#).unwrap();
        assert!(!evaluate(&e, &ctx_basic()).unwrap());
    }

    #[test]
    fn parses_and_or_not_with_parens() {
        let e = parse_expression(
            r#"focused.app_id == "firefox" and (battery.percent < 50 or power.on_battery)"#,
        )
        .unwrap();
        assert!(evaluate(&e, &ctx_basic()).unwrap());

        let e = parse_expression(
            r#"not (focused.app_id == "firefox") or battery.percent > 80"#,
        )
        .unwrap();
        assert!(!evaluate(&e, &ctx_basic()).unwrap());
    }

    #[test]
    fn not_binds_tighter_than_and() {
        // `not a and b` → `(not a) and b`
        let mut ctx = SignalContext::new();
        ctx.set("a", false);
        ctx.set("b", true);
        let e = parse_expression("not a and b").unwrap();
        assert!(evaluate(&e, &ctx).unwrap());
    }

    #[test]
    fn bare_bool_signal_is_truthy() {
        let e = parse_expression("power.on_battery").unwrap();
        assert!(evaluate(&e, &ctx_basic()).unwrap());
    }

    #[test]
    fn missing_signal_in_comparison_is_false() {
        let e = parse_expression("nope.missing == 1").unwrap();
        assert!(!evaluate(&e, &SignalContext::new()).unwrap());
    }

    #[test]
    fn missing_signal_in_truthy_position_is_false() {
        let e = parse_expression("nope.missing").unwrap();
        assert!(!evaluate(&e, &SignalContext::new()).unwrap());
    }

    #[test]
    fn numeric_compare_on_string_is_eval_error() {
        let e = parse_expression(r#"focused.app_id < "z""#).unwrap();
        let err = evaluate(&e, &ctx_basic()).unwrap_err();
        assert!(matches!(err, ContextError::Eval(_)));
    }

    #[test]
    fn parse_error_reports_position() {
        let err = parse_expression("focused.app_id ===").unwrap_err();
        match err {
            ContextError::Parse { position, .. } => assert!(position > 0),
            other => panic!("expected parse error, got {other:?}"),
        }
    }

    #[test]
    fn unterminated_string_is_parse_error() {
        let err = parse_expression(r#"focused.app_id == "firefox"#).unwrap_err();
        assert!(matches!(err, ContextError::Parse { .. }));
    }

    #[test]
    fn negative_number_literal_parses() {
        let mut ctx = SignalContext::new();
        ctx.set("temp.c", -5.0);
        let e = parse_expression("temp.c < 0").unwrap();
        assert!(evaluate(&e, &ctx).unwrap());
        let e = parse_expression("temp.c == -5").unwrap();
        assert!(evaluate(&e, &ctx).unwrap());
    }
}
