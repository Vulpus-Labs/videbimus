//! The expression language of `(expr)"…"` values: numbers, times with
//! unit suffixes, references, `+ − × ÷`, parentheses, and the built-ins
//! `min`, `max`, `clamp` and `frames`. Nothing else: no user functions, no
//! loops, no conditionals (ADR 0005 §3).

use crate::value::{Time, Unit};

/// A byte range within the expression's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub(crate) struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug)]
pub(crate) enum ExprKind {
    Number(f64),
    Time(Time),
    /// The reference, without its `$`.
    Ref(String),
    Neg(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Func {
    Min,
    Max,
    Clamp,
    Frames,
}

impl Func {
    const ALL: [Func; 4] = [Func::Min, Func::Max, Func::Clamp, Func::Frames];

    pub fn name(self) -> &'static str {
        match self {
            Func::Min => "min",
            Func::Max => "max",
            Func::Clamp => "clamp",
            Func::Frames => "frames",
        }
    }
}

#[derive(Debug)]
pub(crate) struct SyntaxError {
    pub message: String,
    pub help: Option<String>,
    pub span: Span,
}

/// How deeply expressions may nest: enough for any config, few enough that
/// a hostile one can't overflow the stack.
const MAX_DEPTH: usize = 64;

pub(crate) fn parse(src: &str) -> Result<Expr, SyntaxError> {
    let mut p = Parser {
        src,
        pos: 0,
        depth: 0,
    };
    let e = p.expr()?;
    p.ws();
    if let Some(c) = p.peek() {
        let fallback = p.error_here(
            format!("unexpected `{c}` after a complete expression"),
            Some(
                "join values with an operator: + - * / (or − × ÷); put spaces around `-` after a name"
                    .into(),
            ),
        );
        return Err(p.stray(fallback));
    }
    Ok(e)
}

/// Whether `path` (without its `$`) is a well-formed reference path.
pub(crate) fn valid_path(path: &str) -> bool {
    let mut p = Parser {
        src: path,
        pos: 0,
        depth: 0,
    };
    p.path_body().is_ok() && p.pos == path.len()
}

struct Parser<'s> {
    src: &'s str,
    pos: usize,
    depth: usize,
}

fn ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.src[self.pos..].chars().next()
    }

    fn peek2(&self) -> Option<char> {
        let mut it = self.src[self.pos..].chars();
        it.next();
        it.next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }

    fn span_from(&self, start: usize) -> Span {
        Span {
            start,
            end: self.pos,
        }
    }

    fn error_here(&self, message: String, help: Option<String>) -> SyntaxError {
        let len = self.peek().map_or(0, char::len_utf8);
        SyntaxError {
            message,
            help,
            span: Span {
                start: self.pos,
                end: self.pos + len,
            },
        }
    }

    /// A better error than `fallback` when what follows is a unit
    /// separated from its number by a space.
    fn stray(&self, fallback: SyntaxError) -> SyntaxError {
        let start = self.pos;
        let len = self.src[start..]
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(self.src.len() - start);
        let word = &self.src[start..start + len];
        if Unit::from_suffix(word).is_some() {
            SyntaxError {
                message: format!("a space separates the unit `{word}` from its number"),
                help: Some(format!("write the unit right after the number: 4{word}")),
                span: Span {
                    start,
                    end: start + len,
                },
            }
        } else {
            fallback
        }
    }

    fn enter(&mut self) -> Result<(), SyntaxError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error_here(
                format!("expression nests more than {MAX_DEPTH} deep"),
                Some("split it into `let` values".into()),
            ));
        }
        Ok(())
    }

    fn expr(&mut self) -> Result<Expr, SyntaxError> {
        self.enter()?;
        let start = self.pos;
        let mut lhs = self.term()?;
        loop {
            self.ws();
            let op = match self.peek() {
                Some('+') => BinOp::Add,
                Some('-' | '−') => BinOp::Sub,
                _ => break,
            };
            self.bump();
            let rhs = self.term()?;
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span: self.span_from(start),
            };
        }
        self.depth -= 1;
        Ok(lhs)
    }

    fn term(&mut self) -> Result<Expr, SyntaxError> {
        self.ws();
        let start = self.pos;
        let mut lhs = self.unary()?;
        loop {
            self.ws();
            let op = match self.peek() {
                Some('*' | '×') => BinOp::Mul,
                Some('/' | '÷') => BinOp::Div,
                _ => break,
            };
            self.bump();
            let rhs = self.unary()?;
            lhs = Expr {
                kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                span: self.span_from(start),
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, SyntaxError> {
        self.ws();
        let start = self.pos;
        if matches!(self.peek(), Some('-' | '−')) {
            self.bump();
            self.enter()?;
            let inner = self.unary()?;
            self.depth -= 1;
            return Ok(Expr {
                kind: ExprKind::Neg(Box::new(inner)),
                span: self.span_from(start),
            });
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, SyntaxError> {
        self.ws();
        let start = self.pos;
        match self.peek() {
            None => Err(self.error_here(
                "expected a value here, but the expression ends".into(),
                Some(
                    "finish the expression with a number, a time such as 4bar, or a $reference"
                        .into(),
                ),
            )),
            Some(c)
                if c.is_ascii_digit()
                    || (c == '.' && self.peek2().is_some_and(|d| d.is_ascii_digit())) =>
            {
                self.number()
            }
            Some('$') => {
                self.bump();
                let body = self.pos;
                self.path_body()?;
                Ok(Expr {
                    kind: ExprKind::Ref(self.src[body..self.pos].to_owned()),
                    span: self.span_from(start),
                })
            }
            Some('(') => {
                self.bump();
                let inner = self.expr()?;
                self.ws();
                if self.peek() != Some(')') {
                    return Err(self.stray(self.error_here(
                        "expected `)`".into(),
                        Some(format!(
                            "close the `(` opened at character {}",
                            self.src[..start].chars().count() + 1
                        )),
                    )));
                }
                self.bump();
                Ok(Expr {
                    kind: inner.kind,
                    span: self.span_from(start),
                })
            }
            Some(c) if ident_start(c) => self.call(),
            Some(c) => Err(self.error_here(
                format!("unexpected `{c}`"),
                Some(
                    "expected a number, a time such as 0.5s, a $reference, a function call or `(`"
                        .into(),
                ),
            )),
        }
    }

    fn number(&mut self) -> Result<Expr, SyntaxError> {
        let start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
            self.bump();
        }
        if self.peek() == Some('.') && self.peek2().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == '_') {
                self.bump();
            }
        }
        let digits: String = self.src[start..self.pos]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        let n: f64 = digits.parse().map_err(|_| SyntaxError {
            message: format!("`{}` isn't a number", &self.src[start..self.pos]),
            help: None,
            span: self.span_from(start),
        })?;
        let unit_start = self.pos;
        while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
            self.bump();
        }
        if unit_start == self.pos {
            return Ok(Expr {
                kind: ExprKind::Number(n),
                span: self.span_from(start),
            });
        }
        let word = &self.src[unit_start..self.pos];
        let Some(unit) = Unit::from_suffix(word) else {
            let close = crate::error::did_you_mean(word, Unit::ALL.map(Unit::suffix));
            return Err(SyntaxError {
                message: format!("unknown unit `{word}`"),
                help: Some(match close {
                    Some(u) => format!("did you mean `{u}`? Units are bar, beat, s and f: {n}{u}"),
                    None => "units are bar, beat, s and f, written right after the number: 4bar, 2beat, 0.5s, 15f".into(),
                }),
                span: self.span_from(unit_start),
            });
        };
        Ok(Expr {
            kind: ExprKind::Time(Time::new(n, unit)),
            span: self.span_from(start),
        })
    }

    /// `name(.name)*`, after the `$`. A `-` continues a name only when a
    /// letter follows it, so `$a-b` is one name and `$a - b` and `$a-1` are
    /// subtractions.
    fn path_body(&mut self) -> Result<(), SyntaxError> {
        loop {
            if !self.peek().is_some_and(ident_start) {
                return Err(self.error_here(
                    "expected a name after `$` or `.`".into(),
                    Some("references look like $tail, $self.start, $parent.length or $prev.camera.end".into()),
                ));
            }
            while let Some(c) = self.peek() {
                if ident_continue(c) || (c == '-' && self.peek2().is_some_and(ident_start)) {
                    self.bump();
                } else {
                    break;
                }
            }
            if self.peek() == Some('.') {
                self.bump();
            } else {
                return Ok(());
            }
        }
    }

    fn call(&mut self) -> Result<Expr, SyntaxError> {
        let start = self.pos;
        while self.peek().is_some_and(ident_continue) {
            self.bump();
        }
        let name = &self.src[start..self.pos];
        let Some(func) = Func::ALL.into_iter().find(|f| f.name() == name) else {
            let help = if Unit::from_suffix(name).is_some() {
                format!("write the unit right after the number, with no space: 4{name}")
            } else if name == "true" || name == "false" || name == "null" {
                "expressions compute numbers and times; write booleans and null as plain KDL values"
                    .into()
            } else {
                let known = crate::error::suggest(name, "built-ins", Func::ALL.map(Func::name));
                format!(
                    "{known}. Config has no user-defined functions (ADR 0005 §3); a reference needs a `$`"
                )
            };
            return Err(SyntaxError {
                message: format!("unknown name `{name}`"),
                help: Some(help),
                span: self.span_from(start),
            });
        };
        self.ws();
        if self.peek() != Some('(') {
            return Err(self.error_here(
                format!("expected `(` after `{name}`"),
                Some(format!("call it: {name}(…)")),
            ));
        }
        self.bump();
        let mut args = Vec::new();
        self.ws();
        if self.peek() == Some(')') {
            self.bump();
        } else {
            loop {
                args.push(self.expr()?);
                self.ws();
                match self.peek() {
                    Some(',') => {
                        self.bump();
                    }
                    Some(')') => {
                        self.bump();
                        break;
                    }
                    _ => {
                        return Err(self.stray(self.error_here(
                            format!("expected `,` or `)` in the call to `{name}`"),
                            None,
                        )));
                    }
                }
            }
        }
        Ok(Expr {
            kind: ExprKind::Call(func, args),
            span: self.span_from(start),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(e: &Expr) -> String {
        match &e.kind {
            ExprKind::Number(n) => format!("{n}"),
            ExprKind::Time(t) => format!("{t}"),
            ExprKind::Ref(r) => format!("${r}"),
            ExprKind::Neg(x) => format!("(-{})", show(x)),
            ExprKind::Binary(op, a, b) => format!("({} {} {})", show(a), op.symbol(), show(b)),
            ExprKind::Call(f, args) => {
                let a: Vec<String> = args.iter().map(show).collect();
                format!("{}({})", f.name(), a.join(", "))
            }
        }
    }

    #[test]
    fn precedence_and_units() {
        let e = parse("$self.length + 0.5s * 2 - 1bar").unwrap();
        assert_eq!(show(&e), "(($self.length + (0.5s * 2)) - 1bar)");
        let e = parse("clamp($a-b, 2beat, (4bar − 1f) ÷ 2) × −3").unwrap();
        assert_eq!(show(&e), "(clamp($a-b, 2beat, ((4bar - 1f) / 2)) * (-3))");
        let e = parse("$a-1").unwrap();
        assert_eq!(show(&e), "($a - 1)");
        assert_eq!(
            show(&parse("frames($prev.camera.end)").unwrap()),
            "frames($prev.camera.end)"
        );
    }

    #[test]
    fn syntax_errors_point_at_the_problem() {
        let e = parse("4 bar").unwrap_err();
        assert_eq!(
            e.message,
            "a space separates the unit `bar` from its number"
        );
        assert_eq!(e.span, Span { start: 2, end: 5 });
        assert!(e.help.unwrap().contains("4bar"));
        let e = parse("2bars").unwrap_err();
        assert!(e.help.unwrap().contains("did you mean `bar`"));
        let e = parse("sqrt(2)").unwrap_err();
        assert!(e.help.unwrap().contains("no user-defined functions"));
        let e = parse("(1 + 2").unwrap_err();
        assert_eq!(e.message, "expected `)`");
        let e = parse("1 +").unwrap_err();
        assert!(e.message.contains("expression ends"));
        let deep = format!("{}1{}", "(".repeat(100), ")".repeat(100));
        assert!(parse(&deep).unwrap_err().message.contains("nests"));
        assert!(valid_path("self.beats-per-bar"));
        assert!(!valid_path("self."));
        assert!(!valid_path("1x"));
    }
}
