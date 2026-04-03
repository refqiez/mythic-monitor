use super::{WithSpan, Span};
use crate::sensing::SensingId;
use crate::sprites::{TypeResolver, ValueResolver};
/// Lexer

#[derive(Debug, Copy, Clone)]
pub enum LexError {
    UnexpectedChar,
    Newline,
    InvalidLiteral,
    NonAscii,
    MalformedIdentifierPath,
}

impl LexError {
    fn unexpected(start: usize, end: usize) -> WithSpan<Self> {
        WithSpan {
            val: LexError::UnexpectedChar,
            span: Span { start, end }
        }
    }

    fn newline(start: usize, end: usize) -> WithSpan<Self> {
        WithSpan {
            val: LexError::Newline,
            span: Span { start, end },
        }
    }

    fn invalid(start: usize, end: usize) -> WithSpan<Self> {
        WithSpan {
            val: LexError::InvalidLiteral,
            span: Span { start, end },
        }
    }

    fn nonascii(start: usize, end: usize) -> WithSpan<Self> {
        WithSpan {
            val: LexError::NonAscii,
            span: Span { start, end },
        }
    }

    fn malformed(start: usize, end: usize) -> WithSpan<Self> {
        WithSpan {
            val: LexError::MalformedIdentifierPath,
            span: Span { start, end },
        }
    }
}


#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Token {
    // literals
    Float(f64),
    Bool(bool),

    // identifiers / keywords
    Ident,
    And,
    Or,
    Xor,
    Not,
    If,
    Else,

    // operators
    Plus,
    Minus,
    Slash,
    Star,
    StarStar,

    Eq,
    EqEq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,

    // punctuation
    LParen,
    RParen,

    EOF,
}

impl Token {
    pub fn repr(&self) -> &'static str {
        use Token::*;
        match self {
            // literals
            Float(_) => "number",
            Bool(_)  => "boolean",

            // identifiers / keywords
            Ident => "identifier",
            And | Or | Xor | Not | If | Else => "keyword",

            // operators
            Plus | Minus | Slash | Star | StarStar => "operator",
            Eq | EqEq | NotEq | Lt | Lte | Gt | Gte  => "comparison",

            // punctuation
            LParen | RParen => "parenthesis",

            EOF => "EOF",
        }
    }
}

pub struct Lexer<'src> {
    src: &'src str,
    bytes: &'src [u8],
    pos: usize,
    len: usize,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            len: src.len(),
        }
    }

    #[inline(always)]
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    #[inline(always)]
    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    #[inline(always)]
    fn span_from(&self, start: usize) -> Span {
        Span {
            start,
            end: self.pos,
        }
    }

    fn skip_whitespace(&mut self) -> Result<(), WithSpan<LexError>> {
        while let Some(b) = self.peek() {
            match b {
                b' ' | b'\t' => { self.bump(); }
                b'\n' | b'\r' => { return Err(LexError::newline(self.pos, self.pos+1)); }
                _ => break,
            }
        }
        Ok(())
    }

    pub fn next_token(&mut self) -> Result<WithSpan<Token>, WithSpan<LexError>> {
        self.skip_whitespace()?;

        let start = self.pos;
        let Some(b) = self.peek() else {
            return Ok(WithSpan {
                val: Token::EOF,
                span: Span { start, end: start },
            });
        };

        use Token::*;

        // punctuation & operators
        let kind = match b {
            b'(' => { self.bump(); LParen }
            b')' => { self.bump(); RParen }
            b'+' => { self.bump(); Plus }
            b'-' => { self.bump(); Minus }
            b'/' => { self.bump(); Slash }
            b'*' => {
                self.bump();
                if self.peek() == Some(b'*') { self.bump(); StarStar } else { Star }
            }
            b'<' => {
                self.bump();
                if self.peek() == Some(b'=') { self.bump(); Lte } else { Lt }
            }
            b'>' => {
                self.bump();
                if self.peek() == Some(b'=') { self.bump(); Gte } else { Gt }
            }
            b'=' => {
                self.bump();
                if self.peek() == Some(b'=') { self.bump(); EqEq } else { Eq }
            }
            b'!' => {
                self.bump();
                if self.peek() == Some(b'=') { self.bump(); NotEq } else {
                    return Err(LexError::unexpected(start, self.pos+1));
                }
            }

            // number literal
            b'0'..=b'9' => {
                self.bump();
                let mut has_dot = false;

                while let Some(c) = self.peek() {
                    match c {
                        b'0'..=b'9' => { self.bump(); }
                        b'.' if !has_dot => { has_dot = true; self.bump(); }
                        _ => break,
                    }
                }

                let Ok(v) = self.src[start .. self.pos].parse::<f64>() else {
                    return Err(LexError::invalid(start, self.pos));
                };

                Float(v)
            }

            // identifier or keyword
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => {
                self.bump(); // consume first char

                // the first segment
                while let Some(c) = self.peek() {
                    if c.is_ascii_alphanumeric() || c == b'_' { self.bump(); } else { break; }
                }

                // trailing segments
                while self.peek() == Some(b'.') {
                    self.bump(); // consume '.'

                    match self.peek() {
                        Some(b'a'..=b'z') | Some(b'A'..=b'Z') | Some(b'_') | Some(b'$') => {
                            self.bump(); // consume first char of next segment
                        }
                        _ => { // Invalid identifier-path (e.g., trailing '.' or bad segment)
                            return Err(LexError::malformed(start, self.pos+1));
                        }
                    }

                    // consume rest of identifier segment
                    while let Some(c) = self.peek() {
                        if c.is_ascii_alphanumeric() || c == b'_' { self.bump(); } else { break; }
                    }
                }

                match &self.src[start..self.pos] {
                    "true" => Bool(true),
                    "false" => Bool(false),
                    "and" => And,
                    "or" => Or,
                    "not" => Not,
                    "if" => If,
                    "else" => Else,
                    _ => Ident,
                }
            }

            _ if b >= 0x80 => {
                return Err(LexError::nonascii(start, self.pos+1));
            }

            _ => {
                return Err(LexError::unexpected(start, self.pos+1));
            }
        };

        Ok(WithSpan {
            val: kind,
            span: self.span_from(start),
        })
    }
}


/// Parser

pub struct Arena<T> {
    nodes: Vec<T>,
}

impl<T> Arena<T> {
    pub fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    pub fn alloc(&mut self, expr: T) -> ExprId {
        let id = self.nodes.len() as u32;
        self.nodes.push(expr);
        ExprId(id)
    }

    pub fn get(&self, id: ExprId) -> &T {
        &self.nodes[id.0 as usize]
    }

    pub fn get_mut(&mut self, id: ExprId) -> &mut T {
        &mut self.nodes[id.0 as usize]
    }
}

#[derive(Debug, Clone)]
pub enum ParseError {
    UnexpectedToken { found: Token, expected: String, },
    Unrecognized(LexError),
}

impl ParseError {
    fn unexpected(tok: WithSpan<Token>, expected: &str) -> WithSpan<Self> {
        WithSpan {
            val: ParseError::UnexpectedToken {
                found: tok.val, expected: expected.to_string(),
            },
            span: tok.span,
        }
    }
}

impl From<WithSpan<LexError>> for WithSpan<ParseError> {
    fn from(e: WithSpan<LexError>) -> Self { e.map(ParseError::Unrecognized) }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ExprId(pub u32);

#[derive(Debug, Clone, Copy)]
pub enum UnaryOp {
    Not,
    Neg,
    Pos,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // arith -> arith
    Add,
    Sub,
    Mul,
    Div,

    // arith -> bool (compare)
    Eqf,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,

    // bool -> bool
    Eqb,
    And,
    Or,
    Xor,
}

#[derive(Debug)]
pub enum Expr {
    Float(f64),
    Bool(bool),
    Ident(Span),
    Sensing(SensingId),

    Unary { op: UnaryOp, expr: ExprId, },
    Binary { lhs: ExprId, op: BinOp, rhs: ExprId, }, // only holds arith ops
    Ternary { cond: ExprId, then_: ExprId, else_: ExprId, },
    // first heading, op is obsolete
    ChainedCmp { heading: Option<ExprId>, op: BinOp, rhs: ExprId }, // only holds binary compare opts
}

fn infix_bp(kind: &Token) -> Option<(u8, u8, BinOp)> {
    Some(match kind {
        Token::Star  => (40, 41, BinOp::Mul),
        Token::Slash => (40, 41, BinOp::Div),
        Token::Plus  => (30, 31, BinOp::Add),
        Token::Minus => (30, 31, BinOp::Sub),

        Token::Eq    => (20, 21, BinOp::Eqf),
        Token::NotEq => (20, 21, BinOp::Ne),
        Token::Lt    => (20, 21, BinOp::Lt),
        Token::Lte   => (20, 21, BinOp::Lte),
        Token::Gt    => (20, 21, BinOp::Gt),
        Token::Gte   => (20, 21, BinOp::Gte),

        Token::And   => (10, 11, BinOp::And),
        Token::Xor   => ( 5,  6, BinOp::Xor),
        Token::Or    => ( 5,  6, BinOp::Or),
        Token::EqEq  => ( 3,  4, BinOp::Eqb),

        _ => return None,
    })
}

pub struct Parser<'src> {
    src: &'src str,
    lexer: Lexer<'src>,
    lookahead: Result<WithSpan<Token>, WithSpan<LexError>>,
    arena_expr: Arena<Expr>,
    arena_span: Arena<Span>,
}

impl<'src> Parser<'src> {
    pub fn new(src: &'src str) -> Self {
        let mut lexer = Lexer::new(src);
        let lookahead = lexer.next_token();
        Self {
            src, lexer, lookahead,
            arena_expr: Arena::new(),
            arena_span: Arena::new(),
        }
    }

    fn peek(&self) -> Result<WithSpan<Token>, WithSpan<ParseError>> {
        self.lookahead.map_err(WithSpan::from)
    }

    fn bump(&mut self) {
        self.lookahead = self.lexer.next_token();
    }

    pub fn parse(mut self) -> Result<(Arena<Expr>, Arena<Span>, Option<ExprId>), WithSpan<ParseError>> {
        if self.peek()?.val == Token::EOF {
            return Ok((self.arena_expr, self.arena_span, None));
        }

        let root = self.parse_expr(0)?;

        match self.peek()? {
            WithSpan { val: Token::EOF, .. } => Ok((self.arena_expr, self.arena_span, Some(root))),
            tok => Err(ParseError::unexpected(tok, "EOF")),
        }
    }

    pub fn alloc(&mut self, span: Span, expr: Expr) -> ExprId {
        self.arena_span.alloc(span);
        self.arena_expr.alloc(expr)
    }

    fn parse_expr(&mut self, min_bp: u8) -> Result<ExprId, WithSpan<ParseError>> {
        use Token::*;

        // prefix parsing
        let tok = self.peek()?;
        let WithSpan { val: kind, span } = tok;

        let mut lhs = match kind {
            Float(v) => {
                self.bump();
                self.alloc(span, Expr::Float(v))
            }

            Bool(v) => {
                self.bump();
                self.alloc(span, Expr::Bool(v))
            }

            Ident => {
                self.bump();
                self.alloc(span, Expr::Ident(span))
            }

            LParen => {
                // let start = span.start;
                self.bump();
                let inner = self.parse_expr(0)?;

                let tok = self.peek()?;
                if tok.val != RParen {
                    return Err(ParseError::unexpected(tok, "')'"));
                }

                self.bump();
                inner
            }

            Not | Plus | Minus => {
                let op = match kind {
                    Not => UnaryOp::Not,
                    Plus => UnaryOp::Pos,
                    Minus => UnaryOp::Neg,
                    _ => unreachable!(),
                };

                let start = span.start;
                self.bump();
                let rhs = self.parse_expr(50)?;
                let end = self.arena_span.get(rhs).end;

                self.alloc(Span { start, end },
                    Expr::Unary { op, expr: rhs },
                )
            }

            _ => return Err(ParseError::unexpected(tok, "lparen, not/+/- or literal")),
        };

        // infix parsing (binary + chained)
        loop {
            // ternary (lowest precedence)
            if self.peek()?.val == Token::If && min_bp <= 1 {
                let start = self.arena_span.get(lhs).start;
                self.bump();

                let cond = self.parse_expr(0)?;

                let tok = self.peek()?;
                if tok.val != Token::Else {
                    return Err(ParseError::unexpected(tok, "'else'"));
                }

                self.bump();
                let else_ = self.parse_expr(0)?;
                let end = self.arena_span.get(else_).end;

                lhs = self.alloc(Span { start, end },
                    Expr::Ternary { cond, then_: lhs, else_, },
                );

                continue;
            }

            let Some((lbp, rbp, op)) = infix_bp(&self.peek()?.val) else {
                break;
            };

            if lbp < min_bp {
                break;
            }

            self.bump();
            let rhs = self.parse_expr(rbp)?;

            let span = Span {
                start: self.arena_span.get(lhs).start,
                end: self.arena_span.get(rhs).end,
            };

            lhs = match op {
                BinOp::Eqf | BinOp::Ne | BinOp::Lt | BinOp::Lte | BinOp::Gt | BinOp::Gte => {
                    match &self.arena_expr.get(lhs) {
                        Expr::ChainedCmp { .. } => { // extend chained cmp
                            self.alloc(span,
                                Expr::ChainedCmp { heading: Some(lhs), op, rhs },
                            )
                        }
                        _ => { // new comparison chain starts
                            let heading = Some(self.alloc(span,
                                Expr::ChainedCmp { heading: None, op: BinOp::Eqf, rhs: lhs }, // op is dummy
                            ));
                            self.alloc(span,
                                Expr::ChainedCmp { heading, op, rhs },
                            )
                        }
                    }
                }

                _ => self.alloc(span,
                    Expr::Binary { lhs, op, rhs, },
                ),
            };
        }

        Ok(lhs)
    }
}


/// Semantics

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Type {
    Bool,
    Float,
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Type::Bool  => write!(f, "boolean"),
            Type::Float => write!(f, "float"),
        }
    }
}

#[derive(Debug)]
pub enum SemanticError {
    UnknownParam,
    UnknownIdentifier,
    TypeMismatch {
        expected: Type,
        found: Type,
    },
}

pub fn semantic_pass<'src>(
    src: &'src str,
    arena_expr: &mut Arena<Expr>,
    arena_span: &Arena<Span>,
    id: ExprId,
    resolver: &mut impl TypeResolver,
) -> Result<Type, WithSpan<SemanticError>> {
    let expr = arena_expr.get(id);
    let mut update: Option<Expr> = None;

    use Expr::*;

    let type_error = |id, et, t| -> WithSpan<SemanticError> {
        WithSpan {
            val: SemanticError::TypeMismatch {
                expected: et,
                found: t,
            },
            span: *arena_span.get(id),
        }
    };

    let ret = match *expr {
        Bool(_) => Ok(Type::Bool),
        Float(_) => Ok(Type::Float),

        Ident(span) => {
            let path = &src[span.start..span.end];
            let (ty, rid) = resolver.resolve_type(path)
                .map_err(|e| e.unframe(span))?;
                update = Some(Sensing(rid));
            Ok(ty)
        }

        Sensing(_) => panic!("Cannot re-sanitize"),

        Unary { op, expr: inner } => {
            let t = semantic_pass(src, arena_expr, arena_span, inner, resolver)?;

            let (et, tt) = match op {
                UnaryOp::Not => (Type::Bool, Type::Bool),
                UnaryOp::Neg | UnaryOp::Pos => (Type::Float, Type::Float),
            };

            if et != t {
                Err(type_error(inner, et, t))
            } else {
                Ok(tt)
            }
        }

        Binary { lhs, op, rhs } => {
            let lt = semantic_pass(src, arena_expr, arena_span, lhs, resolver)?;
            let rt = semantic_pass(src, arena_expr, arena_span, rhs, resolver)?;

            use BinOp::*;

            let (elt, ert, tt) = match op {
                And | Or | Xor | Eqb => (Type::Bool, Type::Bool, Type::Bool),
                Add | Sub | Mul | Div => (Type::Float, Type::Float, Type::Float),
                // Eqf | Ne | Lt | Lte | Gt | Gte => (Type::Float, Type::Float, Type::Bool),
                Eqf | Ne | Lt | Lte | Gt | Gte => panic!("Binary node shouldn't hold comparison"),
            };

            if elt != lt {
                Err(type_error(lhs, elt, lt))
            } else if ert != rt {
                Err(type_error(rhs, ert, rt))
            } else {
                Ok(tt)
            }
        }

        ChainedCmp { .. } => {
            let mut next_id = id;
            let mut prev_op: Option<BinOp> = None;

            loop {
                let ChainedCmp { heading, op, rhs } = *arena_expr.get(next_id) else {
                    panic!("Malformed ChainedCmp");
                };
                let t = semantic_pass(src, arena_expr, arena_span, rhs, resolver)?;

                if let Some(prev_op) = prev_op {
                    let et = if prev_op == BinOp::Eqb { Type::Bool } else { Type::Float };
                    if t != et {
                        return Err(type_error(id, et, t));
                    }
                }

                let Some(heading) = heading else {
                    break;
                };

                let et = if op == BinOp::Eqb { Type::Bool } else { Type::Float };
                if t != et {
                    return Err(type_error(id, et, t));
                }

                prev_op = Some(op);
                next_id = heading;
            }

            Ok(Type::Bool)
        }

        Ternary { cond, then_, else_ } => {
            let ct = semantic_pass(src, arena_expr, arena_span, cond, resolver)?;
            if ct != Type::Bool {
                return Err(type_error(cond, Type::Bool, ct));
            }

            let tt = semantic_pass(src, arena_expr, arena_span, then_, resolver)?;
            let et = semantic_pass(src, arena_expr, arena_span, else_, resolver)?;

            if tt != et {
                return Err(type_error(else_, tt, et));
            }

            Ok(tt)
        }
    };

    if let Some(update) = update {
        *arena_expr.get_mut(id) = update;
    }

    ret
}

pub fn unregister(
    arena: &Arena<Expr>,
    id: ExprId,
    unr: &mut impl FnMut(SensingId),
) {
    use Expr::*;

    match arena.get(id) {
        Bool(_) | Float(_) => (),

        Ident(_) => panic!("Cannot unregister un-sanitized"),
        Sensing(sid) => {
            unr(*sid);
        }

        Unary { expr: inner, .. } => unregister(arena, *inner, unr),

        Binary { lhs, op, rhs } => {
            unregister(arena, *lhs, unr);
            unregister(arena, *rhs, unr);
        }

        ChainedCmp { .. } => {
            let mut next_id = id;
            let mut prev_op: Option<BinOp> = None;

            loop {
                let ChainedCmp { heading, op, rhs } = *arena.get(next_id) else {
                    panic!("Malformed ChainedCmp");
                };

                unregister(arena, rhs, unr);

                let Some(heading) = heading else {
                    break;
                };

                prev_op = Some(op);
                next_id = heading;
            }
        }

        Ternary { cond, then_, else_ } => {
            unregister(arena, *cond, unr);
            unregister(arena, *then_, unr);
            unregister(arena, *else_, unr);
        }
    }
}


/// Eval

#[derive(Clone, Copy)]
pub union Value {
    pub bool: bool,
    pub float: f64,
}

pub fn eval_expr(
    arena: &Arena<Expr>,
    id: ExprId,
    resolver: &impl ValueResolver,
) -> Value { unsafe {
    let expr = arena.get(id);

    match expr {
        Expr::Bool(b) => Value { bool: *b },
        Expr::Float(f) => Value { float: *f },
        Expr::Ident(_span) => panic!("Cannot evaluate un-sanitized Expr"),
        Expr::Sensing(rid) => {
            resolver.resolve_value(*rid)
        }
        Expr::Unary { op, expr: inner } => {
            let val = eval_expr(arena, *inner, resolver);
            match op {
                UnaryOp::Not => Value { bool: ! val.bool },
                UnaryOp::Neg => Value { float: - val.float },
                UnaryOp::Pos => val, // Pos is effectively noop
            }
        }
        Expr::Binary { lhs, op, rhs } => {
            use BinOp::*;
            match op {
                And => {
                    let l = eval_expr(arena, *lhs, resolver).bool;
                    if !l { return Value { bool: false }; } // short cutting
                    eval_expr(arena, *rhs, resolver)
                }
                Or => {
                    let l = eval_expr(arena, *lhs, resolver).bool;
                    if l { return Value { bool: true }; } // short cutting
                    eval_expr(arena, *rhs, resolver)
                }
                Eqb | Xor => {
                    let l = eval_expr(arena, *lhs, resolver).bool;
                    let r = eval_expr(arena, *rhs, resolver).bool;
                    Value { bool: if *op == Eqb { l == r } else { l != r } }
                }
                Add | Sub | Mul | Div => {
                    let l = eval_expr(arena, *lhs, resolver).float;
                    let r = eval_expr(arena, *rhs, resolver).float;
                    let res = match op {
                        Add => l + r,
                        Sub => l - r,
                        Mul => l * r,
                        Div => l / r, // division by zero → inf
                        _ => unreachable!(),
                    };
                    Value { float: res }
                }
                Eqf | Ne | Lt | Lte | Gt | Gte => panic!("Binary node should not hold comparison"),
                // Eqf | Ne | Lt | Lte | Gt | Gte => {
                //     let l = eval_expr(arena, *lhs, resolver).float;
                //     let r = eval_expr(arena, *rhs, resolver).float;
                //     let res = match op {
                //         Eqf => l == r,
                //         Ne  => l != r,
                //         Lt  => l <  r,
                //         Lte => l <= r,
                //         Gt  => l >  r,
                //         Gte => l >= r,
                //         _ => unreachable!(),
                //     };
                //     Value { bool: res }
                // }
            }
        }
        Expr::ChainedCmp { .. } => {
            unsafe fn compute(op: BinOp, next: Value, prev: Value) -> bool {
                match op {
                    BinOp::Lt  => next.float <  prev.float,
                    BinOp::Lte => next.float <= prev.float,
                    BinOp::Gt  => next.float >  prev.float,
                    BinOp::Gte => next.float >= prev.float,
                    BinOp::Ne  => next.float != prev.float,
                    BinOp::Eqf => next.float == prev.float,
                    BinOp::Eqb => next.bool  == prev.bool,
                    _ => panic!("ChainedCmp should not hold non-comparison binop"),
                }
            }

            let mut next_id = id;
            let mut prev: Option<(BinOp, Value)> = None;

            loop {
                let Expr::ChainedCmp { heading, op, rhs } = *arena.get(next_id) else {
                    panic!("Malformed ChainedCmp");
                };

                let next_val = eval_expr(arena, rhs, resolver);

                if let Some((op, prev_val)) = prev {
                    let ok = compute(op, next_val, prev_val);
                    if !ok { return Value { bool: false }; }
                }

                let Some(heading) = heading else {
                    break;
                };

                prev = Some((op, next_val));
                next_id = heading;
            }

            Value { bool: true }
        }
        Expr::Ternary { cond, then_, else_ } => {
            let c = eval_expr(arena, *cond, resolver).bool;
            if c {
                eval_expr(arena, *then_, resolver)
            } else {
                eval_expr(arena, *else_, resolver)
            }
        }
    }
}}

fn fmt_expr_debug<W: std::fmt::Write> (
    src: &str,
    arena: &Arena<Expr>,
    id: ExprId,
    f: &mut W,
    indent: usize,
) -> std::fmt::Result {
    let expr = arena.get(id);
    let pad = |f: &mut W| {
        for _ in 0..indent {
            f.write_str("  ")?;
        }
        Ok(())
    };

    pad(f)?;
    write!(f, "#{:?} ", id)?;

    match &expr {
        Expr::Float(v) => {
            writeln!(f, "Float({})", v)?;
        }

        Expr::Bool(v) => {
            writeln!(f, "Bool({})", v)?;
        }

        Expr::Ident(s) => {
            writeln!(f, "Ident({})", &src[s.start..s.end])?;
        }

        Expr::Sensing(r) => {
            writeln!(f, "Sensing({:?})", r)?;
        }

        Expr::Unary { op, expr } => {
            writeln!(f, "Unary({:?})", op)?;
            fmt_expr_debug(src, arena, *expr, f, indent + 1)?;
        }

        Expr::Binary { lhs, op, rhs } => {
            writeln!(f, "Binary({:?})", op)?;
            fmt_expr_debug(src, arena, *lhs, f, indent + 1)?;
            fmt_expr_debug(src, arena, *rhs, f, indent + 1)?;
        }

        Expr::ChainedCmp { .. } => {
            let mut next_id = id;
            let mut exprs = vec![];
            let mut ops = vec![];
            loop {
                let Expr::ChainedCmp { heading, op, rhs } = *arena.get(next_id) else {
                    panic!("Malformed ChainedCmp");
                };
                exprs.push(rhs);
                ops.push(op);
                let Some(heading) = heading else { break };
                next_id = heading;
            }

            writeln!(f, "ChainedCmp")?;
            for (i, expr_id) in exprs.iter().enumerate() {
                if i > 0 {
                    pad(f)?;
                    writeln!(f, "  op {:?}", ops[i - 1])?;
                }
                fmt_expr_debug(src, arena, *expr_id, f, indent + 1)?;
            }
        }

        Expr::Ternary { cond, then_, else_ } => {
            writeln!(f, "Ternary")?;

            pad(f)?;
            writeln!(f, "  then:")?;
            fmt_expr_debug(src, arena, *then_, f, indent + 2)?;

            pad(f)?;
            writeln!(f, "  cond:")?;
            fmt_expr_debug(src, arena, *cond, f, indent + 2)?;

            pad(f)?;
            writeln!(f, "  else:")?;
            fmt_expr_debug(src, arena, *else_, f, indent + 2)?;
        }
    }

    Ok(())
}

#[derive(Copy, Clone, PartialEq, PartialOrd)]
enum Prec {
    Lowest = 0,
    Ternary,
    Or,
    And,
    Cmp,
    Add,
    Mul,
    Unary,
    Atom,
}

fn binop_prec(op: BinOp) -> Prec {
    match op {
        BinOp::Or | BinOp::Xor => Prec::Or,
        BinOp::And => Prec::And,

        BinOp::Eqf | BinOp::Eqb | BinOp::Ne |
        BinOp::Lt | BinOp::Lte |
        BinOp::Gt | BinOp::Gte => Prec::Cmp,

        BinOp::Add | BinOp::Sub => Prec::Add,
        BinOp::Mul | BinOp::Div => Prec::Mul,
    }
}

fn unary_prec(_: UnaryOp) -> Prec {
    Prec::Unary
}

pub fn pretty_print<'src>(
    f: &mut impl std::fmt::Write,
    src: &str,
    arena: &Arena<Expr>,
    root: Option<ExprId>,
) -> std::fmt::Result {
    match root {
        None => f.write_str("<empty>"),
        Some(id) => {
            fmt_expr_pretty(f, src, arena, id, Prec::Lowest)
        }
    }
}

fn fmt_expr_pretty(
    f: &mut impl std::fmt::Write,
    src: &str,
    arena: &Arena<Expr>,
    id: ExprId,
    parent_prec: Prec,
) -> std::fmt::Result {
    let expr = arena.get(id);

    let (my_prec, needs_parens) = match &expr {
        Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Ident(_)
        | Expr::Sensing(_) => (Prec::Atom, false),

        Expr::Unary { op, .. } =>
            (unary_prec(*op), unary_prec(*op) < parent_prec),

        Expr::Binary { op, .. } =>
            (binop_prec(*op), binop_prec(*op) < parent_prec),

        Expr::ChainedCmp { .. } =>
            (Prec::Cmp, Prec::Cmp < parent_prec),

        Expr::Ternary { .. } =>
            (Prec::Ternary, Prec::Ternary < parent_prec),
    };

    if needs_parens {
        f.write_char('(')?;
    }

    match &expr {
        Expr::Float(v) => {
            write!(f, "{}", v)?;
        }

        Expr::Bool(v) => {
            f.write_str(if *v { "true" } else { "false" })?;
        }

        Expr::Ident(span) => {
            let text = &src[span.start..span.end];
            f.write_str(text)?;
        }

        Expr::Sensing(sid) => {
            write!(f, "{:?}", sid)?;
        }

        Expr::Unary { op, expr } => {
            match op {
                UnaryOp::Not => f.write_str("not "),
                UnaryOp::Neg => f.write_char('-'),
                UnaryOp::Pos => f.write_char('+'),
            }?;
            fmt_expr_pretty(f, src, arena, *expr, my_prec)?;
        }

        Expr::Binary { lhs, op, rhs } => {
            fmt_expr_pretty(f, src, arena, *lhs, my_prec)?;
            f.write_char(' ')?;
            f.write_str(binop_str(*op))?;
            f.write_char(' ')?;
            fmt_expr_pretty(f, src, arena, *rhs, my_prec)?;
        }

        Expr::ChainedCmp { .. } => {
            let mut next_id = id;
            let mut exprs = vec![];
            let mut ops = vec![];
            loop {
                let Expr::ChainedCmp { heading, op, rhs } = *arena.get(next_id) else {
                    panic!("Malformed ChainedCmp");
                };
                exprs.push(rhs);
                ops.push(op);
                let Some(heading) = heading else { break };
                next_id = heading;
            }

            fmt_expr_pretty(f, src, arena, exprs[0], Prec::Cmp)?;
            for (op, rhs) in ops.iter().zip(&exprs[1..]) {
                f.write_char(' ')?;
                f.write_str(binop_str(*op))?;
                f.write_char(' ')?;
                fmt_expr_pretty(f, src, arena, *rhs, Prec::Cmp)?;
            }
        }

        Expr::Ternary { cond, then_, else_ } => {
            fmt_expr_pretty(f, src, arena, *then_, Prec::Ternary)?;
            f.write_str(" if ")?;
            fmt_expr_pretty(f, src, arena, *cond, Prec::Ternary)?;
            f.write_str(" else ")?;
            fmt_expr_pretty(f, src, arena, *else_, Prec::Ternary)?;
        }
    }

    if needs_parens {
        f.write_char(')')?;
    }

    Ok(())
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",

        BinOp::Eqb => "==",
        BinOp::Eqf => "=",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Lte => "<=",
        BinOp::Gt => ">",
        BinOp::Gte => ">=",

        BinOp::And => "and",
        BinOp::Xor => "xor",
        BinOp::Or => "or",
    }
}