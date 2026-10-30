//! Python 3.12's expression grammar (`ast.parse(text, mode="eval")`), to
//! the depth the web's evaluator looks: numbers, unary and binary operators
//! and calls keep their structure; every other node is parsed for its
//! syntax and kept only as its `ast` class name, because the evaluator
//! refuses it as soon as it meets it.

use super::lexer::{SyntaxError, Tok, Token};
use super::num::Num;

/// Deepest tree the evaluator builds and walks. Python's own walk stops
/// near 1,000; this stays far inside a worker thread's stack in a debug
/// build, where the parser's frames are largest.
pub const MAX_DEPTH: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mult,
    Div,
    Mod,
    Pow,
    FloorDiv,
    MatMult,
    LShift,
    RShift,
    BitOr,
    BitXor,
    BitAnd,
}

impl BinOp {
    /// The `ast` class name, as the evaluator's refusal names it.
    pub fn name(self) -> &'static str {
        match self {
            BinOp::Add => "Add",
            BinOp::Sub => "Sub",
            BinOp::Mult => "Mult",
            BinOp::Div => "Div",
            BinOp::Mod => "Mod",
            BinOp::Pow => "Pow",
            BinOp::FloorDiv => "FloorDiv",
            BinOp::MatMult => "MatMult",
            BinOp::LShift => "LShift",
            BinOp::RShift => "RShift",
            BinOp::BitOr => "BitOr",
            BinOp::BitXor => "BitXor",
            BinOp::BitAnd => "BitAnd",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
    UAdd,
    USub,
    Not,
    Invert,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(Num),
    /// A constant that is not a number, as `str()` writes it.
    Const(String),
    Name(String),
    Bin(Box<Expr>, BinOp, Box<Expr>),
    Unary(UnOp, Box<Expr>),
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        keywords: usize,
    },
    /// Any other node (`Compare`, `List`, `Attribute`, ...).
    Other(&'static str),
}

/// Why parsing stopped.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    Syntax(SyntaxError),
    /// Nested past [`MAX_DEPTH`].
    TooDeep,
}

impl From<SyntaxError> for ParseError {
    fn from(e: SyntaxError) -> Self {
        ParseError::Syntax(e)
    }
}

type R = Result<(Expr, u32), ParseError>;

const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

pub fn parse(tokens: Vec<Token>) -> Result<Expr, ParseError> {
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        level: 0,
    };
    let (e, _) = p.expressions()?;
    while p.tok() == &Tok::Newline {
        p.pos += 1;
    }
    if p.tok() != &Tok::End {
        return Err(p.fail());
    }
    Ok(e)
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    level: u32,
}

fn wrap(e: Expr, d: u32) -> R {
    if d > MAX_DEPTH {
        return Err(ParseError::TooDeep);
    }
    Ok((e, d))
}

fn other(name: &'static str) -> R {
    Ok((Expr::Other(name), 1))
}

impl Parser {
    fn tok(&self) -> &Tok {
        self.toks.get(self.pos).map(|t| &t.tok).unwrap_or(&Tok::End)
    }

    fn peek_at(&self, off: usize) -> &Tok {
        self.toks
            .get(self.pos + off)
            .map(|t| &t.tok)
            .unwrap_or(&Tok::End)
    }

    fn line(&self) -> usize {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map(|t| t.line)
            .unwrap_or(1)
    }

    fn fail(&self) -> ParseError {
        self.fail_with("invalid syntax")
    }

    fn fail_with(&self, message: &str) -> ParseError {
        ParseError::Syntax(SyntaxError::new(message, self.line()))
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.tok(), Tok::Op(o) if *o == op)
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.tok(), Tok::Name(n) if n == kw)
    }

    fn expect_op(&mut self, op: &str) -> Result<(), ParseError> {
        if !self.is_op(op) {
            return Err(self.fail());
        }
        self.pos += 1;
        Ok(())
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.level += 1;
        if self.level > MAX_DEPTH {
            return Err(ParseError::TooDeep);
        }
        Ok(())
    }

    fn starts_expression(&self) -> bool {
        match self.tok() {
            Tok::Number(_) | Tok::Imaginary(_) | Tok::Str { .. } => true,
            Tok::Name(n) => {
                !KEYWORDS.contains(&n.as_str())
                    || matches!(
                        n.as_str(),
                        "True" | "False" | "None" | "not" | "lambda" | "await"
                    )
            }
            Tok::Op(o) => matches!(*o, "(" | "[" | "{" | "-" | "+" | "~" | "..."),
            _ => false,
        }
    }

    /// `expressions`: one expression, or a tuple of them.
    fn expressions(&mut self) -> R {
        let first = self.expression()?;
        if !self.is_op(",") {
            return Ok(first);
        }
        while self.is_op(",") {
            self.pos += 1;
            if !self.starts_expression() {
                break;
            }
            self.expression()?;
        }
        other("Tuple")
    }

    fn expression(&mut self) -> R {
        self.enter()?;
        let out = self.expression_inner();
        self.level -= 1;
        out
    }

    fn expression_inner(&mut self) -> R {
        if self.is_kw("lambda") {
            return self.lambda();
        }
        let body = self.disjunction()?;
        if !self.is_kw("if") {
            return Ok(body);
        }
        self.pos += 1;
        self.disjunction()?;
        if !self.is_kw("else") {
            return Err(self.fail_with("expected 'else' after 'if' expression"));
        }
        self.pos += 1;
        self.expression()?;
        other("IfExp")
    }

    fn named_expression(&mut self) -> R {
        let is_name = matches!(self.tok(), Tok::Name(n) if !KEYWORDS.contains(&n.as_str()));
        if is_name && matches!(self.peek_at(1), Tok::Op(":=")) {
            self.pos += 2;
            self.expression()?;
            return other("NamedExpr");
        }
        self.expression()
    }

    /// A named expression or `*expr`; the flag says it was starred.
    fn star_named(&mut self) -> Result<(Expr, u32, bool), ParseError> {
        if self.is_op("*") {
            self.pos += 1;
            self.bitwise_or()?;
            return Ok((Expr::Other("Starred"), 1, true));
        }
        let (e, d) = self.named_expression()?;
        Ok((e, d, false))
    }

    fn lambda(&mut self) -> R {
        self.pos += 1;
        while !self.is_op(":") {
            if self.is_op("*") || self.is_op("**") || self.is_op("/") {
                self.pos += 1;
            }
            if matches!(self.tok(), Tok::Name(n) if !KEYWORDS.contains(&n.as_str())) {
                self.pos += 1;
                if self.is_op("=") {
                    self.pos += 1;
                    self.expression()?;
                }
            }
            if self.is_op(",") {
                self.pos += 1;
            } else if !self.is_op(":") {
                return Err(self.fail());
            }
        }
        self.pos += 1;
        self.expression()?;
        other("Lambda")
    }

    fn disjunction(&mut self) -> R {
        let first = self.conjunction()?;
        if !self.is_kw("or") {
            return Ok(first);
        }
        while self.is_kw("or") {
            self.pos += 1;
            self.conjunction()?;
        }
        other("BoolOp")
    }

    fn conjunction(&mut self) -> R {
        let first = self.inversion()?;
        if !self.is_kw("and") {
            return Ok(first);
        }
        while self.is_kw("and") {
            self.pos += 1;
            self.inversion()?;
        }
        other("BoolOp")
    }

    fn inversion(&mut self) -> R {
        if self.is_kw("not") {
            self.pos += 1;
            self.enter()?;
            let inner = self.inversion();
            self.level -= 1;
            let (e, d) = inner?;
            return wrap(Expr::Unary(UnOp::Not, Box::new(e)), d + 1);
        }
        self.comparison()
    }

    /// The comparison operator at the cursor, consumed.
    fn compare_op(&mut self) -> bool {
        let simple = matches!(self.tok(), Tok::Op(o) if matches!(*o, "<" | ">" | "==" | ">=" | "<=" | "!="))
            || self.is_kw("in");
        if simple {
            self.pos += 1;
            return true;
        }
        if self.is_kw("is") {
            self.pos += 1;
            if self.is_kw("not") {
                self.pos += 1;
            }
            return true;
        }
        if self.is_kw("not") && matches!(self.peek_at(1), Tok::Name(n) if n == "in") {
            self.pos += 2;
            return true;
        }
        false
    }

    fn comparison(&mut self) -> R {
        let first = self.bitwise_or()?;
        if !self.compare_op() {
            return Ok(first);
        }
        loop {
            self.bitwise_or()?;
            if !self.compare_op() {
                break;
            }
        }
        other("Compare")
    }

    /// One left-associative binary level.
    fn binary(&mut self, ops: &[(&str, BinOp)], next: fn(&mut Self) -> R) -> R {
        let (mut left, mut depth) = next(self)?;
        loop {
            let found = ops.iter().find(|(o, _)| self.is_op(o)).map(|(_, b)| *b);
            let Some(op) = found else {
                return Ok((left, depth));
            };
            self.pos += 1;
            let (right, rd) = next(self)?;
            depth = depth.max(rd) + 1;
            if depth > MAX_DEPTH {
                return Err(ParseError::TooDeep);
            }
            left = Expr::Bin(Box::new(left), op, Box::new(right));
        }
    }

    fn bitwise_or(&mut self) -> R {
        self.binary(&[("|", BinOp::BitOr)], Self::bitwise_xor)
    }

    fn bitwise_xor(&mut self) -> R {
        self.binary(&[("^", BinOp::BitXor)], Self::bitwise_and)
    }

    fn bitwise_and(&mut self) -> R {
        self.binary(&[("&", BinOp::BitAnd)], Self::shift)
    }

    fn shift(&mut self) -> R {
        self.binary(&[("<<", BinOp::LShift), (">>", BinOp::RShift)], Self::sum)
    }

    fn sum(&mut self) -> R {
        self.binary(&[("+", BinOp::Add), ("-", BinOp::Sub)], Self::term)
    }

    fn term(&mut self) -> R {
        self.binary(
            &[
                ("*", BinOp::Mult),
                ("/", BinOp::Div),
                ("//", BinOp::FloorDiv),
                ("%", BinOp::Mod),
                ("@", BinOp::MatMult),
            ],
            Self::factor,
        )
    }

    fn factor(&mut self) -> R {
        let op = match self.tok() {
            Tok::Op("+") => Some(UnOp::UAdd),
            Tok::Op("-") => Some(UnOp::USub),
            Tok::Op("~") => Some(UnOp::Invert),
            _ => None,
        };
        let Some(op) = op else {
            return self.power();
        };
        self.pos += 1;
        self.enter()?;
        let inner = self.factor();
        self.level -= 1;
        let (e, d) = inner?;
        wrap(Expr::Unary(op, Box::new(e)), d + 1)
    }

    fn power(&mut self) -> R {
        let (base, bd) = self.await_primary()?;
        if !self.is_op("**") {
            return Ok((base, bd));
        }
        self.pos += 1;
        self.enter()?;
        let exp = self.factor();
        self.level -= 1;
        let (exp, ed) = exp?;
        wrap(
            Expr::Bin(Box::new(base), BinOp::Pow, Box::new(exp)),
            bd.max(ed) + 1,
        )
    }

    fn await_primary(&mut self) -> R {
        if self.is_kw("await") {
            self.pos += 1;
            self.primary()?;
            return other("Await");
        }
        self.primary()
    }

    fn primary(&mut self) -> R {
        let (mut e, mut d) = self.atom()?;
        loop {
            if self.is_op(".") {
                self.pos += 1;
                if !matches!(self.tok(), Tok::Name(n) if !KEYWORDS.contains(&n.as_str())) {
                    return Err(self.fail());
                }
                self.pos += 1;
                (e, d) = (Expr::Other("Attribute"), 1);
            } else if self.is_op("(") {
                let (args, keywords, ad) = self.call_args()?;
                d = d.max(ad) + 1;
                if d > MAX_DEPTH {
                    return Err(ParseError::TooDeep);
                }
                e = Expr::Call {
                    func: Box::new(e),
                    args,
                    keywords,
                };
            } else if self.is_op("[") {
                self.subscript()?;
                (e, d) = (Expr::Other("Subscript"), 1);
            } else {
                return Ok((e, d));
            }
        }
    }

    fn call_args(&mut self) -> Result<(Vec<Expr>, usize, u32), ParseError> {
        self.pos += 1;
        let mut args = Vec::new();
        let mut keywords = 0usize;
        let mut depth = 0;
        while !self.is_op(")") {
            if self.is_op("*") {
                self.pos += 1;
                self.expression()?;
                args.push(Expr::Other("Starred"));
            } else if self.is_op("**") {
                self.pos += 1;
                self.expression()?;
                keywords += 1;
            } else if matches!(self.tok(), Tok::Name(n) if !KEYWORDS.contains(&n.as_str()))
                && matches!(self.peek_at(1), Tok::Op("="))
            {
                self.pos += 2;
                self.expression()?;
                keywords += 1;
            } else {
                if keywords > 0 {
                    return Err(self.fail_with("positional argument follows keyword argument"));
                }
                let (mut e, mut d) = self.named_expression()?;
                if self.is_kw("for") || self.is_kw("async") {
                    self.comprehension()?;
                    if !args.is_empty() || !self.is_op(")") {
                        return Err(self.fail_with("Generator expression must be parenthesized"));
                    }
                    (e, d) = (Expr::Other("GeneratorExp"), 1);
                }
                depth = depth.max(d);
                args.push(e);
            }
            if self.is_op(",") {
                self.pos += 1;
            } else if !self.is_op(")") {
                return Err(self.fail());
            }
        }
        self.pos += 1;
        Ok((args, keywords, depth))
    }

    fn subscript(&mut self) -> Result<(), ParseError> {
        self.pos += 1;
        if self.is_op("]") {
            return Err(self.fail());
        }
        while !self.is_op("]") {
            if !self.is_op(":") {
                self.star_named()?;
            }
            if self.is_op(":") {
                self.pos += 1;
                if self.starts_expression() {
                    self.expression()?;
                }
                if self.is_op(":") {
                    self.pos += 1;
                    if self.starts_expression() {
                        self.expression()?;
                    }
                }
            }
            if self.is_op(",") {
                self.pos += 1;
            } else if !self.is_op("]") {
                return Err(self.fail());
            }
        }
        self.pos += 1;
        Ok(())
    }

    fn comprehension(&mut self) -> Result<(), ParseError> {
        while self.is_kw("for") || self.is_kw("async") {
            if self.is_kw("async") {
                self.pos += 1;
            }
            if !self.is_kw("for") {
                return Err(self.fail());
            }
            self.pos += 1;
            loop {
                if self.is_op("*") {
                    self.pos += 1;
                }
                self.bitwise_or()?;
                if !self.is_op(",") {
                    break;
                }
                self.pos += 1;
                if self.is_kw("in") {
                    break;
                }
            }
            if !self.is_kw("in") {
                return Err(self.fail());
            }
            self.pos += 1;
            self.disjunction()?;
            while self.is_kw("if") {
                self.pos += 1;
                self.disjunction()?;
            }
        }
        Ok(())
    }

    fn atom(&mut self) -> R {
        let tok = self.tok().clone();
        match tok {
            Tok::Number(n) => {
                self.pos += 1;
                Ok((Expr::Num(n), 1))
            }
            Tok::Imaginary(f) => {
                self.pos += 1;
                Ok((Expr::Const(Num::Complex(0.0, f).text()), 1))
            }
            Tok::Str { .. } => self.strings(),
            Tok::Name(n) => {
                self.pos += 1;
                match n.as_str() {
                    "True" | "False" | "None" => Ok((Expr::Const(n), 1)),
                    k if KEYWORDS.contains(&k) => {
                        self.pos -= 1;
                        Err(self.fail())
                    }
                    _ => Ok((Expr::Name(n), 1)),
                }
            }
            Tok::Op("...") => {
                self.pos += 1;
                Ok((Expr::Const("Ellipsis".into()), 1))
            }
            Tok::Op("(") => self.group(),
            Tok::Op("[") => self.list(),
            Tok::Op("{") => self.braces(),
            _ => Err(self.fail()),
        }
    }

    /// Adjacent string literals, joined as Python joins them.
    fn strings(&mut self) -> R {
        let mut text = String::new();
        let mut joined = false;
        let mut bytes: Option<bool> = None;
        while let Tok::Str { text: t, bytes: b } = self.tok().clone() {
            if bytes.is_some_and(|prev| prev != b) {
                return Err(self.fail_with("cannot mix bytes and nonbytes literals"));
            }
            bytes = Some(b);
            match t {
                Some(t) => text.push_str(&t),
                None => joined = true,
            }
            self.pos += 1;
        }
        if joined {
            return other("JoinedStr");
        }
        if bytes == Some(true) {
            return Ok((Expr::Const(super::lexer::bytes_repr(&text)), 1));
        }
        Ok((Expr::Const(text), 1))
    }

    fn group(&mut self) -> R {
        self.pos += 1;
        if self.is_op(")") {
            self.pos += 1;
            return other("Tuple");
        }
        if self.is_kw("yield") {
            self.pos += 1;
            if self.is_kw("from") {
                self.pos += 1;
                self.expression()?;
            } else if self.starts_expression() || self.is_op("*") {
                self.star_named()?;
                while self.is_op(",") {
                    self.pos += 1;
                    if self.is_op(")") {
                        break;
                    }
                    self.star_named()?;
                }
            }
            self.expect_op(")")?;
            return other("Yield");
        }
        let (first, depth, starred) = self.star_named()?;
        if self.is_kw("for") || self.is_kw("async") {
            if starred {
                return Err(self.fail());
            }
            self.comprehension()?;
            self.expect_op(")")?;
            return other("GeneratorExp");
        }
        if self.is_op(",") {
            while self.is_op(",") {
                self.pos += 1;
                if self.is_op(")") {
                    break;
                }
                self.star_named()?;
            }
            self.expect_op(")")?;
            return other("Tuple");
        }
        self.expect_op(")")?;
        if starred {
            return Err(self.fail_with("cannot use starred expression here"));
        }
        Ok((first, depth))
    }

    fn list(&mut self) -> R {
        self.pos += 1;
        if self.is_op("]") {
            self.pos += 1;
            return other("List");
        }
        self.star_named()?;
        if self.is_kw("for") || self.is_kw("async") {
            self.comprehension()?;
            self.expect_op("]")?;
            return other("ListComp");
        }
        while self.is_op(",") {
            self.pos += 1;
            if self.is_op("]") {
                break;
            }
            self.star_named()?;
        }
        self.expect_op("]")?;
        other("List")
    }

    fn braces(&mut self) -> R {
        self.pos += 1;
        if self.is_op("}") {
            self.pos += 1;
            return other("Dict");
        }
        let is_dict = if self.is_op("**") {
            self.pos += 1;
            self.bitwise_or()?;
            true
        } else {
            let (_, _, starred) = self.star_named()?;
            if self.is_op(":") && !starred {
                self.pos += 1;
                self.expression()?;
                if self.is_kw("for") || self.is_kw("async") {
                    self.comprehension()?;
                    self.expect_op("}")?;
                    return other("DictComp");
                }
                true
            } else {
                if self.is_kw("for") || self.is_kw("async") {
                    self.comprehension()?;
                    self.expect_op("}")?;
                    return other("SetComp");
                }
                false
            }
        };
        while self.is_op(",") {
            self.pos += 1;
            if self.is_op("}") {
                break;
            }
            if is_dict {
                if self.is_op("**") {
                    self.pos += 1;
                    self.bitwise_or()?;
                } else {
                    self.expression()?;
                    self.expect_op(":")?;
                    self.expression()?;
                }
            } else {
                self.star_named()?;
            }
        }
        self.expect_op("}")?;
        other(if is_dict { "Dict" } else { "Set" })
    }
}
