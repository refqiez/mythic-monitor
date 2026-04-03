use super::{Span, Pos, WithPos, message_with_evidence};

/// Lexical error

#[derive(Debug, Clone, Copy)]
pub enum LexError {
    UnexpectedEof,
    InvalidNumber,
    UnexpectedChar,
}

/// Parsing error
#[derive(Debug, Clone)]
pub enum ParseError {
    UnexpectedToken { found: &'static str, expected: String },
    Unrecognized(LexError),
}

impl ParseError {
    fn unexpected_(tok: WithPos<Token>, expected: String) -> WithPos<Self> {
        tok.map(|tok| ParseError::UnexpectedToken {
            found: tok.repr(), expected,
        })
    }

    fn unexpected(tok: WithPos<Token>, expected: &str) -> WithPos<Self> {
        Self::unexpected_(tok, expected.to_string())
    }

    pub fn message_with_evidence(&self,
        f: &mut std::fmt::Formatter,
        file: &str,
        lineno: usize,
        buf: &str,
        span: Option<Span>,
    ) -> std::fmt::Result {
        use log::Level::*;
        use ParseError::*;
        match self {
            UnexpectedToken { found, expected } => {
                // TODO remove self.lieno?
                message_with_evidence(f, Error, file, lineno, buf, span,
                    format_args!("unexpected {}, expecting {}", found, expected)
                )
            }
            Unrecognized(lex_error) => {
                use LexError::*;
                let msg = match lex_error {
                    UnexpectedEof => "unexpected EOF",
                    InvalidNumber => "invalid number",
                    UnexpectedChar => "unexpected char",
                };
                message_with_evidence(f, Error, file, lineno, buf, span,
                    format_args!("{}", msg)
                )
            }
        }
    }
}

impl From<WithPos<LexError>> for WithPos<ParseError> {
    fn from(e: WithPos<LexError>) -> Self { e.map(ParseError::Unrecognized) }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Token<'src> {
    Identifier(&'src str),
    Number(f64),
    Boolean(bool),
    String(&'src str),
    Equals,
    Comma,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Newline,
    Eof,
}

impl Token<'_> {
    pub fn repr(&self) -> &'static str {
        use Token::*;
        match self {
            Identifier(_) => "identifier",
            Number(_) => "number",
            Boolean(_) => "boolean",
            String(_) => "string",
            Equals => "assignment",
            Comma => "comma",
            LBracket => "'['",
            RBracket => "']'",
            LBrace => "'{'",
            RBrace => "'}'",
            Newline => "newline",
            Eof => "EOF",
        }
    }
}

pub struct Lexer<'src> {
    input: &'src str,
    chars: std::str::CharIndices<'src>,
    peeked: Option<(usize, char)>,
    line: usize,
    column: usize,
    offset: usize,
}

impl<'src> Lexer<'src> {
    pub fn new(input: &'src str) -> Self {
        Self {
            input,
            chars: input.char_indices(),
            peeked: None,
            line: 1,
            column: 1,
            offset: 0,
        }
    }

    fn peek(&mut self) -> Option<(usize, char)> {
        if self.peeked.is_none() {
            self.peeked = self.chars.next();
        }
        self.peeked
    }

    fn next_char(&mut self) -> Option<(usize, char)> {
        let next = if let Some(c) = self.peeked.take() { Some(c) } else { self.chars.next() };
        if let Some((idx, ch)) = next {
            self.offset = idx;
            if ch == '\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
            Some((idx, ch))
        } else {
            None
        }
    }

    fn skip_ws(&mut self) {
        while let Some((_, ch)) = self.peek() {
            if ch == ' ' || ch == '\t' {
                self.next_char();
            } else {
                break;
            }
        }
    }

    fn skip_comment(&mut self) {
        while let Some((_, ch)) = self.peek() {
            if ch == '\n' {
                break;
            }
            self.next_char();
        }
    }

    pub fn next_token(&mut self) -> Result<WithPos<Token<'src>>, WithPos<LexError>> {
        fn is_ident_start(ch: char) -> bool { ch.is_ascii_alphabetic() || ch == '_' }
        fn is_ident_char(ch: char) -> bool { ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' }

        self.skip_ws();

        // consume till newline or eof
        if let Some((_, '#')) = self.peek() {
            self.skip_comment();
        }

        let mut pos = Pos {
            line: self.line, column: self.column,
            span: Span::nil(),
        };

        let Some((start, c)) = self.next_char() else {
            pos.span = Span { start: self.offset, end: self.offset };
            return Ok(WithPos { val: Token::Eof, pos, });
        };
        let end = if let Some((end, _)) = self.peek() { end } else {
            self.input.len()
        };
        pos.span = Span { start, end, }; // default to 1-char width

        match c {
            '\n'=> Ok(WithPos { pos, val: Token::Newline }),
            '\r'=> Ok(WithPos { pos, val: Token::Newline }),
            '=' => Ok(WithPos { pos, val: Token::Equals }),
            ',' => Ok(WithPos { pos, val: Token::Comma }),
            '[' => Ok(WithPos { pos, val: Token::LBracket }),
            ']' => Ok(WithPos { pos, val: Token::RBracket }),
            '{' => Ok(WithPos { pos, val: Token::LBrace }),
            '}' => Ok(WithPos { pos, val: Token::RBrace }),
            '"' => {
                let s_start = start + 1;
                while let Some((_, ch)) = self.next_char() {
                    if ch == '"' { break; }
                }
                let s_end = self.offset;
                if s_end < s_start {
                    return Err(WithPos { val: LexError::UnexpectedEof, pos });
                }
                let s = &self.input[s_start..s_end];
                pos.span = Span { start, end: s_end+1 }; // include following '"'
                Ok(WithPos { val: Token::String(s), pos })
            }
            ch if is_ident_start(ch) => {
                let mut end = start + ch.len_utf8();
                while let Some((idx, ch2)) = self.peek() {
                    if is_ident_char(ch2) {
                        self.next_char();
                        end = idx + ch2.len_utf8();
                    } else {
                        break;
                    }
                }
                let s = &self.input[start..end];
                let kind = match s {
                    // TODO Context-Aware lexing?
                    "true" => Token::Boolean(true),
                    "false" => Token::Boolean(false),
                    _ => Token::Identifier(s),
                };
                pos.span = Span { start, end };
                Ok(WithPos { val: kind, pos })
            }
            ch if ch.is_ascii_digit() || ch == '+' || ch == '-' => {
                let mut end = start + ch.len_utf8();
                while let Some((idx, ch2)) = self.peek() {
                    if ch2.is_ascii_digit() || ch2 == '.' {
                        self.next_char();
                        end = idx + ch2.len_utf8();
                    } else {
                        break;
                    }
                }
                let s = &self.input[start..end];
                pos.span = Span { start, end };
                let num: f64 = s.parse().map_err(|_| WithPos { val: LexError::InvalidNumber, pos })?;
                Ok(WithPos { val: Token::Number(num), pos })
            }
            _ => Err(WithPos { val: LexError::UnexpectedChar, pos }),
        }
    }
}


/// Parser

#[derive(Debug, Clone)]
pub enum Value<'src> {
    String(&'src str),
    Number(f64),
    Boolean(bool),
    Array(Vec<WithPos<Value<'src>>>),
    Table(Table<'src>),
}

impl<'src> Value<'src> {
    pub fn type_str(&self) -> &'static str {
        use Value::*;
        match self {
            String(_)  => "string",
            Number(_)  => "number",
            Boolean(_) => "boolean",
            Array(_)   => "array",
            Table(_)   => "table",
        }
    }

    pub fn extract<'v, T: ExtractValue<'src> + ?Sized>(&'v self) -> Result<&'v T, ExtractError> {
        T::extract_from_toml_value(self)
    }

    pub fn extract_mut<'v, T: ExtractValue<'src> + ?Sized>(&'v mut self) -> Result<&'v mut T, ExtractError> {
        T::extract_mut_from_toml_value(self)
    }
}

pub trait ExtractValue<'src> {
    fn extract_from_toml_value<'v>(v: &'v Value<'src>) -> Result<&'v Self, ExtractError>;
    fn extract_mut_from_toml_value<'v>(v: &'v mut Value<'src>) -> Result<&'v mut Self, ExtractError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtractError {
    pub expected: &'static str,
    pub found: &'static str,
}

macro_rules! extract_value_impl {
    ($t:ty, $name:literal, $pat:tt) => {
        impl<'src> ExtractValue<'src> for $t {
            fn extract_from_toml_value<'v>(v: &'v Value<'src>) -> Result<&'v Self, ExtractError> {
                match v {
                    Value::$pat(v) => Ok(v),
                    _ => Err(ExtractError {expected: $name, found: v.type_str()}),
                }
            }

            fn extract_mut_from_toml_value<'v>(v: &'v mut Value<'src>) -> Result<&'v mut Self, ExtractError> {
                match v {
                    Value::$pat(v) => Ok(v),
                    _ => Err(ExtractError {expected: $name, found: v.type_str()}),
                }
            }
        }
    };
}

extract_value_impl!{&'src str, "string", String}
extract_value_impl!{f64, "number", Number}
extract_value_impl!{bool, "boolean", Boolean}
extract_value_impl!{Vec<WithPos<Value<'src>>>, "array", Array}
extract_value_impl!{Table<'src>, "table", Table}

// pos of section points to the start of the section header
// pos of key-value point to the start of the value
#[derive(Debug, Clone)]
pub struct Entry<'src> {
    pub key: WithPos<&'src str>,
    pub val: WithPos<Value<'src>>,
}

#[derive(Debug, Clone)]
pub struct Table<'src>(pub Vec<Entry<'src>>);

/// Parser
pub struct Parser<'src> {
    lexer: Lexer<'src>,
    peeked: Option<Result<WithPos<Token<'src>>, WithPos<LexError>>>,
}

impl<'src> Parser<'src> {
    pub fn new(input: &'src str) -> Self {
        Self { lexer: Lexer::new(input), peeked: None }
    }

    fn peek(&mut self) -> Result<WithPos<Token<'src>>, WithPos<ParseError>> {
        if self.peeked.is_none() { // if initial peek
            self.peeked = Some(self.lexer.next_token());
        }
        match self.peeked {
            Some(Ok(tok)) => Ok(tok),
            Some(Err(e)) => Err(WithPos::from(e)),
            None => unreachable!(),
        }
    }

    fn next(&mut self) -> Result<WithPos<Token<'src>>, WithPos<ParseError>> {
        Ok(if let Some(tok) = self.peeked.take() {
            tok?
        } else {
            self.lexer.next_token()?
        })
    }

    fn expect(&mut self, kind: Token<'src>) -> Result<WithPos<Token<'src>>, WithPos<ParseError>> {
        let tok = self.next()?;
        if tok.val != kind {
            return Err(ParseError::unexpected_(tok, format!("{:?}", kind)));
        }
        Ok(tok)
    }

    pub fn parse(mut self) -> Result<Table<'src>, WithPos<ParseError>> {
        let mut root: Table<'src> = Table(vec![]);
        // let mut sections: Vec<(String, Table<'src>, Pos)> = vec![];
        let mut current_table: Option<usize> = None;

        while self.peek()?.val != Token::Eof {
            let tok = self.peek()?;
            match tok.val {
                Token::Newline => { self.next()?; }
                Token::LBracket => {
                    let (name, bigpos) = self.parse_section_header()?;
                    let entry = Entry { key: name, val: WithPos { pos: bigpos, val: Value::Table(Table(vec![])) }};
                    root.0.push(entry);
                    current_table = Some(root.0.len()-1);
                }
                Token::Identifier(_) => {
                    let entry = self.parse_key_value()?;
                    if let Some(idx) = current_table {
                        let Value::Table(table) = &mut root.0[idx].val.val else { unreachable!() };
                        table.0.push(entry);
                    } else {
                        root.0.push(entry);
                    }
                }
                _ => return Err(ParseError::unexpected(tok, "identifier or sectoin header")),
            }
        }

        Ok(root)
    }

    fn parse_section_header(&mut self) -> Result<(WithPos<&'src str>, Pos), WithPos<ParseError>> {
        let start = self.expect(Token::LBracket)?.pos.span.start;
        let name = self.next()?;
        let name = name.map(|tok| match tok {
            Token::Identifier(s) => Ok(s),
            _ => return Err(ParseError::unexpected(name, "section name")),
        }).traverse()?;
        let end = self.expect(Token::RBracket)?.pos.span.end;
        if let Token::Newline = self.peek()?.val {
            self.next()?;
        }
        let bigpos = Pos { span: Span { start, end}, ..name.pos };
        Ok((name, bigpos))
    }

    fn parse_key_value(&mut self) -> Result<Entry<'src>, WithPos<ParseError>> {
        let key = self.next()?;
        let key = key.map(|tok| match tok {
            Token::Identifier(s) => Ok(s),
            _ => return Err(ParseError::unexpected(key, "identifier")),
        }).traverse()?;
        self.expect(Token::Equals)?;
        let val = self.parse_value()?;
        if let Token::Newline = self.peek()?.val {
            self.next()?;
        }
        Ok(Entry { key, val })
    }

    fn parse_value(&mut self) -> Result<WithPos<Value<'src>>, WithPos<ParseError>> {
        let peek = self.peek()?;
        match peek.val {
            Token::String(s)  => { self.next()?; Ok(WithPos { pos: peek.pos, val: Value::String(s) }) }
            Token::Number(n)  => { self.next()?; Ok(WithPos { pos: peek.pos, val: Value::Number(n) }) }
            Token::Boolean(b) => { self.next()?; Ok(WithPos { pos: peek.pos, val: Value::Boolean(b)}) }
            Token::LBracket => self.parse_array(),
            Token::LBrace => self.parse_inline_table(),
            _ => Err(ParseError::unexpected(peek, "value")),
        }
    }

    fn parse_array(&mut self) -> Result<WithPos<Value<'src>>, WithPos<ParseError>> {
        let mut pos = self.expect(Token::LBracket)?.pos;
        let mut elements = vec![];

        let end= loop {
            let peek = self.peek()?;
            if peek.val == Token::RBracket {
                break self.next()?.pos;
            }
            if peek.val == Token::Newline {
                return Err(ParseError::unexpected(peek, "value"));
            }
            elements.push(self.parse_value()?);

            let peek = self.peek()?;
            if peek.val == Token::Comma {
                self.next()?;
            } else {
                if self.peek()?.val != Token::RBracket {
                    return Err(ParseError::unexpected(peek, "comma or ']'"));
                }
            }
        };

        pos.span.end = end.span.end;
        Ok(WithPos { val: Value::Array(elements), pos })
    }

    fn parse_inline_table(&mut self) -> Result<WithPos<Value<'src>>, WithPos<ParseError>> {
        let mut pos = self.expect(Token::LBrace)?.pos;
        let mut table = vec![];

        let end = loop {
            let peek = self.peek()?;
            if peek.val == Token::RBrace {
                break self.next()?.pos;
            }
            if peek.val == Token::Newline {
                return Err(ParseError::unexpected(peek, "value"));
            }

            let key = self.next()?;
            let key = key.map(|tok| match tok {
                Token::Identifier(s) => Ok(s),
                _ => Err(ParseError::unexpected(key, "identifier")),
            }).traverse()?;
            self.expect(Token::Equals)?;
            let val = self.parse_value()?;
            table.push(Entry { key, val });

            let peek = self.peek()?;
            if peek.val == Token::Comma {
                self.next()?;
            } else if peek.val != Token::RBrace {
                return Err(ParseError::unexpected(peek, "comma or '}'"));
            }
        };

        pos.span.end = end.span.end;
        Ok(WithPos { val: Value::Table(Table(table)), pos })
    }
}

#[derive(Debug)]
pub enum RetrieveError<'src> {
    FieldNotFound(&'src str), // key
    IncompatibleType(&'static str, &'static str), // key, expected, found
}

impl<'src> Table<'src> {
    pub fn get(&self, key: &str) -> Option<&Entry<'src>> {
        self.0.iter().rfind(|e| e.key.val == key)
    }

    pub fn get_all<'p>(&self, key: &'p str) -> impl Iterator<Item=&Entry<'src>> + use <'_, 'src, 'p>{
        self.0.iter().filter(move |e| e.key.val == key)
    }

    pub fn pop(&mut self, key: &str) -> Option<Entry<'src>> {
        let idx = self.0.iter().rposition(|e| e.key.val == key)?;
        Some(self.0.remove(idx))
    }

    pub fn pop_all<'p>(&mut self, key: &'p str) -> impl Iterator<Item=Entry<'src>> + use <'_, 'src, 'p>{
        self.0.extract_if(.., move |e| e.key.val == key)
    }

    pub fn retrieve<'t, T: ExtractValue<'src> + ?Sized>(&'t self, key: &'src str) -> Result<WithPos<&'t T>, WithPos<RetrieveError<'src>>> {
        let Some(entry) = self.get(key) else {
            return Err(WithPos::nil(RetrieveError::FieldNotFound(key)))
        };

        entry.val.val.extract::<T>().map_err(move |e|
            WithPos { pos: entry.val.pos, val: RetrieveError::IncompatibleType(e.expected, e.found) }
        ).map(|val| WithPos {pos: entry.val.pos, val })
    }

    pub fn get_all_with_prefix<'p>(&self, prefix: &'p str) -> impl Iterator<Item=&Entry<'src>> + use<'_, 'src, 'p> {
        self.0.iter().filter(move |e| e.key.val.starts_with(prefix))
    }

    pub fn pop_all_with_prefix<'p>(&mut self, prefix: &'p str) -> impl Iterator<Item=Entry<'src>> + use<'_, 'src, 'p> {
        self.0.extract_if(.., move |e| e.key.val.starts_with(prefix))
    }
}