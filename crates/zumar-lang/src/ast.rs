//! The zumar-lang v0 AST. Deliberately small: one model record, payload-less
//! messages, per-message update equations with record-update semantics, and
//! an expression language of Int/String/Bool with `if`, `show`, and `++`.
//!
//! This AST is the backend seam: `gen` (Rust) consumes it today; a WasmGC
//! backend consumes the same tree later.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ty {
    Int,
    Str,
    Bool,
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Ty::Int => "Int",
            Ty::Str => "String",
            Ty::Bool => "Bool",
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Str(String),
    Bool(bool),
    /// `model.<field>`
    Field(String, Pos),
    /// `show(<Int expr>)` -> String
    Show(Box<Expr>, Pos),
    Bin(Op, Box<Expr>, Box<Expr>, Pos),
    If(Box<Expr>, Box<Expr>, Box<Expr>, Pos),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Concat,
    Eq,
    Lt,
    Gt,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Attr {
    /// `class "counter"`, `placeholder "..."` — any string attribute.
    Str { name: String, value: Expr, pos: Pos },
    /// `onClick MsgName`
    OnClick { msg: String, pos: Pos },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Child {
    Elem(Element),
    /// `text <String expr>`
    Text(Expr, Pos),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<Attr>,
    pub children: Vec<Child>,
}

/// A `{ field = expr, ... }` record literal (used by `init` and `update`).
pub type Record = Vec<(String, Expr, Pos)>;

#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub name: String,
    pub model: Vec<(String, Ty, Pos)>,
    pub init: Record,
    pub msgs: Vec<(String, Pos)>,
    /// `update <Msg> = { field = expr, ... }`; unlisted fields keep their
    /// old value (record-update semantics).
    pub updates: Vec<(String, Record, Pos)>,
    pub view: Element,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ZuError {
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

impl ZuError {
    pub fn at(pos: Pos, msg: impl Into<String>) -> ZuError {
        ZuError {
            line: pos.line,
            col: pos.col,
            msg: msg.into(),
        }
    }
}

impl std::fmt::Display for ZuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: error: {}", self.line, self.col, self.msg)
    }
}
