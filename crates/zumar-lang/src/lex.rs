use crate::ast::{Pos, ZuError};

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(i64),
    Str(String),
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Comma,
    Colon,
    Eq,   // =
    EqEq, // ==
    Ne,   // !=
    Pipe,
    Dot,
    Plus,
    PlusPlus,
    Minus,
    Star,
    Lt,
    Gt,
    Eof,
}

impl std::fmt::Display for Tok {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tok::Ident(s) => write!(f, "`{s}`"),
            Tok::Int(n) => write!(f, "`{n}`"),
            Tok::Str(_) => write!(f, "string literal"),
            Tok::Eof => write!(f, "end of file"),
            t => write!(
                f,
                "`{}`",
                match t {
                    Tok::LBrace => "{",
                    Tok::RBrace => "}",
                    Tok::LBracket => "[",
                    Tok::RBracket => "]",
                    Tok::LParen => "(",
                    Tok::RParen => ")",
                    Tok::Comma => ",",
                    Tok::Colon => ":",
                    Tok::Eq => "=",
                    Tok::EqEq => "==",
                    Tok::Ne => "!=",
                    Tok::Pipe => "|",
                    Tok::Dot => ".",
                    Tok::Plus => "+",
                    Tok::PlusPlus => "++",
                    Tok::Minus => "-",
                    Tok::Star => "*",
                    Tok::Lt => "<",
                    Tok::Gt => ">",
                    _ => unreachable!(),
                }
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub pos: Pos,
}

/// Lex the whole source. `#` starts a line comment.
pub fn lex(src: &str) -> Result<Vec<Token>, ZuError> {
    let mut out = Vec::new();
    let mut line = 1usize;
    let mut col = 1usize;
    let mut chars = src.chars().peekable();

    macro_rules! push {
        ($tok:expr, $pos:expr) => {
            out.push(Token {
                tok: $tok,
                pos: $pos,
            })
        };
    }

    while let Some(&c) = chars.peek() {
        let pos = Pos { line, col };
        match c {
            '\n' => {
                chars.next();
                line += 1;
                col = 1;
            }
            ' ' | '\t' | '\r' => {
                chars.next();
                col += 1;
            }
            '#' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        line += 1;
                        col = 1;
                        break;
                    }
                }
            }
            '{' | '}' | '[' | ']' | '(' | ')' | ',' | ':' | '|' | '.' | '*' | '<' | '>' | '-' => {
                chars.next();
                col += 1;
                push!(
                    match c {
                        '{' => Tok::LBrace,
                        '}' => Tok::RBrace,
                        '[' => Tok::LBracket,
                        ']' => Tok::RBracket,
                        '(' => Tok::LParen,
                        ')' => Tok::RParen,
                        ',' => Tok::Comma,
                        ':' => Tok::Colon,
                        '|' => Tok::Pipe,
                        '.' => Tok::Dot,
                        '*' => Tok::Star,
                        '<' => Tok::Lt,
                        '>' => Tok::Gt,
                        '-' => Tok::Minus,
                        _ => unreachable!(),
                    },
                    pos
                );
            }
            '=' => {
                chars.next();
                col += 1;
                if chars.peek() == Some(&'=') {
                    chars.next();
                    col += 1;
                    push!(Tok::EqEq, pos);
                } else {
                    push!(Tok::Eq, pos);
                }
            }
            '+' => {
                chars.next();
                col += 1;
                if chars.peek() == Some(&'+') {
                    chars.next();
                    col += 1;
                    push!(Tok::PlusPlus, pos);
                } else {
                    push!(Tok::Plus, pos);
                }
            }
            '!' => {
                chars.next();
                col += 1;
                if chars.peek() == Some(&'=') {
                    chars.next();
                    col += 1;
                    push!(Tok::Ne, pos);
                } else {
                    return Err(ZuError::at(
                        pos,
                        "unexpected `!` (did you mean `!=`, or `not`?)",
                    ));
                }
            }
            '"' => {
                chars.next();
                col += 1;
                let mut s = String::new();
                loop {
                    match chars.next() {
                        None | Some('\n') => {
                            return Err(ZuError::at(pos, "unterminated string literal"))
                        }
                        Some('"') => {
                            col += 1;
                            break;
                        }
                        Some('\\') => {
                            col += 2;
                            match chars.next() {
                                Some('n') => s.push('\n'),
                                Some('"') => s.push('"'),
                                Some('\\') => s.push('\\'),
                                other => {
                                    return Err(ZuError::at(
                                        Pos { line, col },
                                        format!("unknown escape `\\{}`", other.unwrap_or(' ')),
                                    ))
                                }
                            }
                        }
                        Some(c) => {
                            col += 1;
                            s.push(c);
                        }
                    }
                }
                push!(Tok::Str(s), pos);
            }
            c if c.is_ascii_digit() => {
                let mut n: i64 = 0;
                while let Some(&d) = chars.peek() {
                    if let Some(digit) = d.to_digit(10) {
                        chars.next();
                        col += 1;
                        n = n
                            .checked_mul(10)
                            .and_then(|n| n.checked_add(digit as i64))
                            .ok_or_else(|| ZuError::at(pos, "integer literal overflows Int"))?;
                    } else {
                        break;
                    }
                }
                push!(Tok::Int(n), pos);
            }
            c if c.is_ascii_alphabetic() || c == '_' => {
                let mut s = String::new();
                while let Some(&a) = chars.peek() {
                    if a.is_ascii_alphanumeric() || a == '_' {
                        chars.next();
                        col += 1;
                        s.push(a);
                    } else {
                        break;
                    }
                }
                push!(Tok::Ident(s), pos);
            }
            c => return Err(ZuError::at(pos, format!("unexpected character `{c}`"))),
        }
    }
    out.push(Token {
        tok: Tok::Eof,
        pos: Pos { line, col },
    });
    Ok(out)
}
