//! The condition expression language.
//!
//! A small unsigned-integer expression language over named values. The
//! grammar, precedence, and evaluation semantics are the ones in the
//! technical design section 6.2:
//!
//! - precedence low to high: `||`, `&&`, comparison, additive,
//!   multiplicative, unary `!`, primary;
//! - division and remainder by zero evaluate to 0;
//! - comparisons evaluate to 1 or 0;
//! - arithmetic wraps at u32.
//!
//! The evaluator reads identifiers from a caller-provided [`Scope`].

use std::collections::BTreeMap;

/// An evaluation failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EvalError {
    /// The token stream is not a valid expression.
    #[error("expression syntax error at {offset}: {message}")]
    Syntax {
        /// The byte offset in the source text.
        offset: usize,
        /// The problem.
        message: String,
    },

    /// An identifier is not in scope.
    #[error("unknown identifier: {0}")]
    UnknownIdent(String),
}

/// The name-to-value bindings for one evaluation.
pub struct Scope<'a> {
    values: &'a BTreeMap<String, u64>,
}

impl<'a> Scope<'a> {
    /// A scope over a name-to-value table.
    pub fn new(values: &'a BTreeMap<String, u64>) -> Self {
        Self { values }
    }
}

/// A parsed expression. Parse once, evaluate many times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// A numeric literal.
    Num(u64),
    /// A named value.
    Ident(String),
    /// Unary not.
    Not(Box<Expr>),
    /// Binary operators.
    Bin {
        /// The operator.
        op: BinOp,
        /// The left operand.
        left: Box<Expr>,
        /// The right operand.
        right: Box<Expr>,
    },
}

/// The binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// Logical or.
    Or,
    /// Logical and.
    And,
    /// Equal.
    Eq,
    /// Not equal.
    Ne,
    /// Less than.
    Lt,
    /// Less or equal.
    Le,
    /// Greater than.
    Gt,
    /// Greater or equal.
    Ge,
    /// Add.
    Add,
    /// Subtract.
    Sub,
    /// Multiply.
    Mul,
    /// Divide.
    Div,
    /// Remainder.
    Rem,
}

/// Parse an expression text into an [`Expr`].
///
/// # Errors
///
/// Returns a syntax error when the text does not match the grammar.
pub fn parse_expr(text: &str) -> Result<Expr, EvalError> {
    let tokens = tokenize(text)?;
    let mut pos = 0usize;
    let expr = parse_or(&tokens, &mut pos)?;
    if pos != tokens.len() {
        return Err(EvalError::Syntax {
            offset: tokens[pos].1,
            message: "trailing tokens after the expression".to_string(),
        });
    }
    Ok(expr)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Num(u64),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
}

fn tokenize(text: &str) -> Result<Vec<(Token, usize)>, EvalError> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            if text[i..].starts_with("0x") || text[i..].starts_with("0X") {
                i += 2;
                while i < bytes.len() && (bytes[i] as char).is_ascii_hexdigit() {
                    i += 1;
                }
                let raw = &text[start..i];
                let value = u64::from_str_radix(&raw[2..], 16).map_err(|_| EvalError::Syntax {
                    offset: start,
                    message: format!("bad hex literal: {raw}"),
                })?;
                out.push((Token::Num(value), start));
            } else {
                while i < bytes.len() && (bytes[i] as char).is_ascii_digit() {
                    i += 1;
                }
                let raw = &text[start..i];
                let value = raw.parse::<u64>().map_err(|_| EvalError::Syntax {
                    offset: start,
                    message: format!("bad decimal literal: {raw}"),
                })?;
                out.push((Token::Num(value), start));
            }
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < bytes.len() {
                let ch = bytes[i] as char;
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                    i += 1;
                } else {
                    break;
                }
            }
            out.push((Token::Ident(text[start..i].to_string()), start));
        } else if text[i..].starts_with("&&") {
            out.push((Token::Op("&&"), i));
            i += 2;
        } else if text[i..].starts_with("||") {
            out.push((Token::Op("||"), i));
            i += 2;
        } else if text[i..].starts_with("==") {
            out.push((Token::Op("=="), i));
            i += 2;
        } else if text[i..].starts_with("!=") {
            out.push((Token::Op("!="), i));
            i += 2;
        } else if text[i..].starts_with("<=") {
            out.push((Token::Op("<="), i));
            i += 2;
        } else if text[i..].starts_with(">=") {
            out.push((Token::Op(">="), i));
            i += 2;
        } else if text[i..].starts_with('<') {
            out.push((Token::Op("<"), i));
            i += 1;
        } else if text[i..].starts_with('>') {
            out.push((Token::Op(">"), i));
            i += 1;
        } else if "+-*/%!".contains(c) {
            out.push((
                Token::Op(match c {
                    '+' => "+",
                    '-' => "-",
                    '*' => "*",
                    '/' => "/",
                    '%' => "%",
                    _ => "!",
                }),
                i,
            ));
            i += 1;
        } else if c == '(' {
            out.push((Token::LParen, i));
            i += 1;
        } else if c == ')' {
            out.push((Token::RParen, i));
            i += 1;
        } else {
            return Err(EvalError::Syntax {
                offset: i,
                message: format!("unexpected character: {c}"),
            });
        }
    }
    Ok(out)
}

type Tokens<'t> = [(Token, usize)];

fn parse_or(tokens: &Tokens<'_>, pos: &mut usize) -> Result<Expr, EvalError> {
    let mut left = parse_and(tokens, pos)?;
    while matches!(peek(tokens, pos), Some(Token::Op("||"))) {
        *pos += 1;
        let right = parse_and(tokens, pos)?;
        left = Expr::Bin {
            op: BinOp::Or,
            left: Box::new(left),
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_and(tokens: &Tokens<'_>, pos: &mut usize) -> Result<Expr, EvalError> {
    let mut left = parse_cmp(tokens, pos)?;
    while matches!(peek(tokens, pos), Some(Token::Op("&&"))) {
        *pos += 1;
        let right = parse_cmp(tokens, pos)?;
        left = Expr::Bin {
            op: BinOp::And,
            left: Box::new(left),
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_cmp(tokens: &Tokens<'_>, pos: &mut usize) -> Result<Expr, EvalError> {
    let left = parse_sum(tokens, pos)?;
    let op = match peek(tokens, pos) {
        Some(Token::Op("==")) => BinOp::Eq,
        Some(Token::Op("!=")) => BinOp::Ne,
        Some(Token::Op("<")) => BinOp::Lt,
        Some(Token::Op("<=")) => BinOp::Le,
        Some(Token::Op(">")) => BinOp::Gt,
        Some(Token::Op(">=")) => BinOp::Ge,
        _ => return Ok(left),
    };
    *pos += 1;
    let right = parse_sum(tokens, pos)?;
    Ok(Expr::Bin {
        op,
        left: Box::new(left),
        right: Box::new(right),
    })
}

fn parse_sum(tokens: &Tokens<'_>, pos: &mut usize) -> Result<Expr, EvalError> {
    let mut left = parse_term(tokens, pos)?;
    loop {
        let op = match peek(tokens, pos) {
            Some(Token::Op("+")) => BinOp::Add,
            Some(Token::Op("-")) => BinOp::Sub,
            _ => break,
        };
        *pos += 1;
        let right = parse_term(tokens, pos)?;
        left = Expr::Bin {
            op,
            left: Box::new(left),
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_term(tokens: &Tokens<'_>, pos: &mut usize) -> Result<Expr, EvalError> {
    let mut left = parse_factor(tokens, pos)?;
    loop {
        let op = match peek(tokens, pos) {
            Some(Token::Op("*")) => BinOp::Mul,
            Some(Token::Op("/")) => BinOp::Div,
            Some(Token::Op("%")) => BinOp::Rem,
            _ => break,
        };
        *pos += 1;
        let right = parse_factor(tokens, pos)?;
        left = Expr::Bin {
            op,
            left: Box::new(left),
            right: Box::new(right),
        };
    }
    Ok(left)
}

fn parse_factor(tokens: &Tokens<'_>, pos: &mut usize) -> Result<Expr, EvalError> {
    match peek(tokens, pos) {
        Some(Token::Op("!")) => {
            *pos += 1;
            let inner = parse_factor(tokens, pos)?;
            Ok(Expr::Not(Box::new(inner)))
        }
        Some(Token::LParen) => {
            *pos += 1;
            let inner = parse_or(tokens, pos)?;
            match peek(tokens, pos) {
                Some(Token::RParen) => {
                    *pos += 1;
                    Ok(inner)
                }
                _ => Err(EvalError::Syntax {
                    offset: tokens[*pos - 1].1,
                    message: "missing closing parenthesis".to_string(),
                }),
            }
        }
        Some(Token::Num(v)) => {
            let e = Expr::Num(*v);
            *pos += 1;
            Ok(e)
        }
        Some(Token::Ident(name)) => {
            let e = Expr::Ident(name.clone());
            *pos += 1;
            Ok(e)
        }
        _ => Err(EvalError::Syntax {
            offset: peek_offset(tokens, pos),
            message: "expected a value".to_string(),
        }),
    }
}

fn peek<'t>(tokens: &'t [(Token, usize)], pos: &usize) -> Option<&'t Token> {
    tokens.get(*pos).map(|(t, _)| t)
}

fn peek_offset(tokens: &[(Token, usize)], pos: &usize) -> usize {
    tokens.get(*pos).map(|(_, o)| *o).unwrap_or(usize::MAX)
}

impl Expr {
    /// Evaluate the expression against a scope.
    ///
    /// # Errors
    ///
    /// Returns an error when an identifier is not in scope.
    pub fn eval(&self, scope: &Scope<'_>) -> Result<u64, EvalError> {
        match self {
            Expr::Num(v) => Ok(*v),
            Expr::Ident(name) => scope
                .values
                .get(name)
                .copied()
                .ok_or_else(|| EvalError::UnknownIdent(name.clone())),
            Expr::Not(inner) => Ok((inner.eval(scope)? == 0) as u64),
            Expr::Bin { op, left, right } => {
                let a = left.eval(scope)?;
                let b = right.eval(scope)?;
                Ok(match op {
                    BinOp::Or => ((a != 0) || (b != 0)) as u64,
                    BinOp::And => ((a != 0) && (b != 0)) as u64,
                    BinOp::Eq => (a == b) as u64,
                    BinOp::Ne => (a != b) as u64,
                    BinOp::Lt => (a < b) as u64,
                    BinOp::Le => (a <= b) as u64,
                    BinOp::Gt => (a > b) as u64,
                    BinOp::Ge => (a >= b) as u64,
                    BinOp::Add => a.wrapping_add(b),
                    BinOp::Sub => a.wrapping_sub(b),
                    BinOp::Mul => a.wrapping_mul(b),
                    // Division by zero evaluates to 0 by design.
                    BinOp::Div => a.checked_div(b).unwrap_or(0),
                    BinOp::Rem => a.checked_rem(b).unwrap_or(0),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn eval_text(text: &str, values: &[(&str, u64)]) -> u64 {
        let map: BTreeMap<String, u64> = values.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let expr = parse_expr(text).expect("parses");
        expr.eval(&Scope::new(&map)).expect("evaluates")
    }

    #[test]
    fn literals_and_hex() {
        assert_eq!(eval_text("42", &[]), 42);
        assert_eq!(eval_text("0xFF", &[]), 255);
        assert_eq!(eval_text("0x10 * 2", &[]), 32);
    }

    #[test]
    fn precedence_chain() {
        // 1 + 2 * 3 == 7, not 9.
        assert_eq!(eval_text("1 + 2 * 3", &[]), 7);
        // (1 + 2) * 3 == 9.
        assert_eq!(eval_text("(1 + 2) * 3", &[]), 9);
        // Comparison binds tighter than &&.
        assert_eq!(eval_text("1 == 1 && 2 < 3", &[]), 1);
        assert_eq!(eval_text("1 == 2 || 2 <= 2", &[]), 1);
    }

    #[test]
    fn identifiers_and_unary_not() {
        assert_eq!(eval_text("A == 0x3F", &[("A", 0x3F)]), 1);
        assert_eq!(eval_text("!(A == 0)", &[("A", 5)]), 1);
        assert_eq!(eval_text("!0", &[]), 1);
        assert_eq!(eval_text("!1", &[]), 0);
    }

    #[test]
    fn division_by_zero_is_zero() {
        assert_eq!(eval_text("5 / 0", &[]), 0);
        assert_eq!(eval_text("5 % 0", &[]), 0);
        assert_eq!(eval_text("HL / 2", &[("HL", 8)]), 4);
    }

    #[test]
    fn comparisons_evaluate_to_zero_or_one() {
        assert_eq!(eval_text("3 != 3", &[]), 0);
        assert_eq!(eval_text("3 >= 3", &[]), 1);
        assert_eq!(eval_text("HL < 0x8000", &[("HL", 0x7FFF)]), 1);
    }

    #[test]
    fn unknown_ident_fails() {
        let map = BTreeMap::new();
        let expr = parse_expr("NOPE + 1").unwrap();
        match expr.eval(&Scope::new(&map)) {
            Err(EvalError::UnknownIdent(name)) => assert_eq!(name, "NOPE"),
            other => panic!("expected UnknownIdent, got {other:?}"),
        }
    }

    #[test]
    fn syntax_errors_report_offsets() {
        assert!(parse_expr("1 +").is_err());
        assert!(parse_expr("(1 + 2").is_err());
        assert!(parse_expr("1 & 2").is_err());
        assert!(parse_expr("1 @ 2").is_err());
    }

    #[test]
    fn device_qualified_identifiers() {
        assert_eq!(eval_text("ppu.count == 4", &[("ppu.count", 4)]), 1);
    }

    #[test]
    fn wrap_around_arithmetic() {
        // u64 arithmetic; the design says u32-width registers declare their
        // own masks, so the language itself computes at u64.
        assert_eq!(eval_text("0xFFFFFFFF + 1", &[]), 4294967296);
        assert_eq!(eval_text("0 - 1", &[]), 0xFFFF_FFFF_FFFF_FFFF);
    }
}
