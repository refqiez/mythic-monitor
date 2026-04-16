#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span { // half open range of byte offsets in the buffer
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub line: usize,
    pub column: usize,
    pub span: Span,
}

impl Span {
    pub fn nil() -> Self { Self { start: 0, end: 0 }}
    pub fn whole(s: &str) -> Self { Self { start: 0, end: s.len() } }
    pub fn ending(s: &str) -> Self { Self { start: s.len(), end: s.len() } }

    pub fn with<T>(&self, val: T) -> WithSpan<T> {
        WithSpan {
            span: *self,
            val,
        }
    }

    // should later be implemented with Pattern once it is stable in rustc
    pub fn split<'a>(orig: &'a str, pat: char) -> impl Iterator<Item=Span> + use<'a> {
        orig.split(pat).map(|s| {
            let start = unsafe { s.as_ptr().offset_from(orig.as_ptr()) } as usize;
            Span { start, end: start + s.len() }
        })
    }

    /// Reverse of Span::nest.
    /// If B is included in A:
    ///   A.frame(B).unframe(A) == B
    pub fn unframe(&self, parent: Span) -> Self {
        *self + parent.start
    }

    /// Consider two Spans:
    ///   A = 0 [1  2  3  4  5] 6  == {start:1, end:6}
    ///   B = 0  1  2 [3  4] 5  6  == {start:3, end:5}
    /// A.frame(B) = {start:2, end:4}
    /// the result will be clamped if out of range
    pub fn frame(&self, nest: Span) -> Self {
        let parent_len = self.end - self.start;
        let start = nest.start.saturating_sub(self.start);
        let end = std::cmp::min(parent_len, nest.end.saturating_sub(self.start));
        Span { start, end }
    }

    pub fn slice<'a>(&self, s: &'a str) -> &'a str {
        &s[self.start .. self.end]
    }
}

impl std::ops::Add<usize> for Span {
    type Output = Self;
    fn add(self, rhs: usize) -> Self { Self { start: self.start + rhs, end: self.end + rhs } }
}

impl std::ops::Sub<usize> for Span {
    type Output = Self;
    fn sub(self, rhs: usize) -> Self { Self { start: self.start - rhs, end: self.end - rhs } }
}

impl Pos {
    pub fn nil() -> Self {
        Pos { line: 0, column: 0, span: Span::nil() }
    }

    pub fn with<T>(&self, val: T) -> WithPos<T> {
        WithPos {
            pos: *self,
            val,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithSpan<T> {
    pub val: T,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WithPos<T> {
    pub val: T,
    pub pos: Pos
}

impl<T> WithSpan<T> {
    pub fn nil(val: T) -> Self { Self { span: Span::nil(), val, } }

    pub fn map<S>(self: WithSpan<T>, f: impl FnOnce(T) -> S) -> WithSpan<S> {
        WithSpan {
            span: self.span,
            val: f(self.val),
        }
    }

    pub fn unframe(self, parent: Span) -> Self {
        Self {
            val: self.val,
            span: self.span.unframe(parent),
        }
    }

    pub fn into<S>(self) -> WithSpan<S> where T: Into<S> {
        self.map(T::into)
    }
}

impl<T> WithPos<T> {
    pub fn nil(val: T) -> Self { Self { pos: Pos::nil(), val, } }

    pub fn map<S>(self, f: impl FnOnce(T) -> S) -> WithPos<S> {
        WithPos {
            pos: self.pos,
            val: f(self.val),
        }
    }

    pub fn into<S>(self) -> WithPos<S> where T: Into<S> {
        self.map(T::into)
    }
}

impl<T,S> WithPos<Result<T,S>> {
    pub fn traverse(self) -> Result<WithPos<T>, S> {
        Ok(WithPos{ pos: self.pos, val: self.val? })
    }
}


// find a line containing character of offset from the src,
// find within-line offset
pub fn lineview(src: &str, span: Span) -> (&str, Span) {
    let linestart = src[..span.start].rfind('\n').map(|i| i+1).unwrap_or(0);
    let lineend = src[span.start..].find('\n').map(|x| x + span.start).unwrap_or(src.len());
    let buf = &src[linestart..lineend];
    (buf, span - linestart)
}

// writes rustc style error message
// e.g.
// > error: type alias takes 0 generic arguments but 1 generic argument was supplied
// >   --> app\src\sprite\controller.rs:27:25
// >    |
// > 27 | type FmtRet = std::fmt::Result<()>;
// >    |                         ^^^^^^
// assumes buf is a single line, without trailing newline
// assumes buffer contains only single-width characters.
// support for wide characters(using unicode-width crate) is a no-goal for now.
pub fn message_with_evidence(
    f: &mut std::fmt::Formatter,
    level: log::Level,
    file: &str,
    lineno: usize,
    buf: &str, // source of the situation
    span: Option<Span>, // byte offset in the buf for the range of interest
    message: std::fmt::Arguments,
) -> std::fmt::Result {
    use std::cmp::{min, max};

    fn write_pad(f: &mut std::fmt::Formatter, pad: u32) -> std::fmt::Result {
        for _ in 0 .. pad { write!(f, " ")?; } Ok(())
    }

    writeln!(f, "{}: {message}", crate::base::logger::level_as_str(level))?;

    let pad = 3 + if lineno == 0 {0} else {lineno.ilog(10)};
    write_pad(f, pad-1)?;
    writeln!(f, "--> {file}:{lineno}")?;

    let Some(span) = span else { return Ok(()) };
    let span = {
        let mut span_normalized = if buf.len() <= span.start {
            Span {
                start: buf.len(),
                end: buf.len(),
            }
        } else {
            Span {
                start: span.start,
                end: min(buf.len(), max(span.start, span.end)),
            }
        };

        if span_normalized.start == span_normalized.end {
            span_normalized.end = span_normalized.start + 1;
        }

        if span_normalized != span {
            writeln!(f, "::invalid span range {span:?}, report to the developer.")?;
        }

        span_normalized
    };

    let (bufstart, bufend) = if buf.len() < 80 {
        // the line fits the screen width
        (0, buf.len())
    } else {
        // the line is too long, finding proper cut points
        // this will result in 70-cols substring
        let spanlen = span.end - span.start;
        if spanlen > 50 {
            // even the span is too long, focusing on the span start
            let start = buf.ceil_char_boundary(span.start.saturating_sub(20));
            let end = buf.floor_char_boundary(start + 50);
            (start, end)
        } else {
            // the span fits the screen, find proper margine
            let margine = 70 - spanlen / 2;
            if span.end + margine > buf.len() {
                // not enough right magine
                let end = buf.len();
                let lmargine = margine + margine - (buf.len() - span.end);
                let start = buf.ceil_char_boundary(span.start - lmargine);
                (start, end)
            } else if span.start < margine {
                // not enough left magine
                let start = 0;
                let rmargine  = margine + margine - span.start;
                let end = buf.floor_char_boundary(span.end + rmargine);
                (start, end)
            } else {
                let start = buf.ceil_char_boundary(span.start - margine);
                let end = buf.floor_char_boundary(span.end + margine);
                (start, end)
            }
        }
    };

    write_pad(f, pad)?;
    writeln!(f, "|")?;

    write_pad(f,1)?;
    write!(f, "{lineno} | ")?;
    if bufstart > 0 { write!(f, "...")?; }
    write!(f, "{}", &buf[bufstart..bufend])?;
    if bufend < buf.len() { write!(f, "...")?; }
    writeln!(f)?;

    let cutspan = Span {
        start: max(bufstart, span.start),
        end: min(bufend, span.end),
    };
    let mut spancol = cutspan - bufstart;
    if 0 < bufstart { // bufstart is cut
        spancol.start += 3; // displace with leading "..."
        spancol.end += 3; // displace with leading "..."
        if span.start < cutspan.start { // span start is cut
            spancol.start -= 3; // include leading "..."
        }
    }
    if cutspan.end < span.end { // span end is cut (buf end is also cut)
        spancol.end += 3; // include following "..."
    }

    write_pad(f, pad)?;
    write!(f, "| ")?;
    write_pad(f, spancol.start as u32)?;
    for _ in spancol.start .. spancol.end { write!(f, "^")?; }
    writeln!(f)?;

    Ok(())
}

// use base::write_report instead
// pub struct MessageWithEvidence<'a> {
//     pub level: log::Level,
//     pub file: &'a str,
//     pub lineno: usize,
//     pub buf: &'a str,
//     pub span: Option<Span>,
//     pub message: &'a str,
// }

// impl<'a> std::fmt::Display for MessageWithEvidence<'a> {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         message_with_evidence(f, self.level, self.file, self.lineno, self.buf, self.span, |f| write!(f, "{}", self.message))
//     }
// }

pub mod expr;
pub mod toml;