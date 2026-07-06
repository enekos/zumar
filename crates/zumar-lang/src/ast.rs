//! The zumar-lang AST.
//!
//! v0.2 adds record types, `List`, message payloads, and a small functional
//! layer (comprehensions, record literals/updates) so real list-shaped apps
//! compile. The AST is the backend seam: `gen` (Rust) consumes it today; a
//! WasmGC backend consumes the same tree later.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Str,
    Bool,
    List(Box<Ty>),
    /// A named record — either a `record` declaration or the reserved
    /// `Model` synthesized from the `model {}` block.
    Record(String),
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::Int => f.write_str("Int"),
            Ty::Str => f.write_str("String"),
            Ty::Bool => f.write_str("Bool"),
            Ty::List(t) => write!(f, "List {t}"),
            Ty::Record(n) => f.write_str(n),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Str(String),
    Bool(bool),
    /// A bound name: `model`, a comprehension variable, or a message payload.
    Var(String, Pos),
    /// `<base>.<field>`
    Field(Box<Expr>, String, Pos),
    /// `show(<Int>)` -> String
    Show(Box<Expr>, Pos),
    /// `length(<List>)` -> Int
    Len(Box<Expr>, Pos),
    /// `reverse(<List T>)` -> List T
    Reverse(Box<Expr>, Pos),
    /// `not <Bool>`
    Not(Box<Expr>, Pos),
    Bin(Op, Box<Expr>, Box<Expr>, Pos),
    If(Box<Expr>, Box<Expr>, Box<Expr>, Pos),
    /// `[a, b, c]`
    ListLit(Vec<Expr>, Pos),
    /// `{ field = e, ... }` — resolves to a record by its field set.
    RecordLit(Vec<(String, Expr, Pos)>, Pos),
    /// `{ base | field = e, ... }`
    RecordUpdate(Box<Expr>, Vec<(String, Expr, Pos)>, Pos),
    /// `for <var> in <list> [where <cond>] yield <body>` -> List
    For {
        var: String,
        list: Box<Expr>,
        cond: Option<Box<Expr>>,
        body: Box<Expr>,
        pos: Pos,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Concat,
    Eq,
    Ne,
    Lt,
    Gt,
}

/// A message value in event position: `Reverse` or `Delete(t.id)`.
#[derive(Debug, Clone, PartialEq)]
pub struct MsgCall {
    pub name: String,
    pub arg: Option<Expr>,
    pub pos: Pos,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ValueKind {
    /// `event.target.value` -> String constructor
    Value,
    /// `event.target.checked` -> Bool constructor
    Checked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Attr {
    /// `class "x"`, `value model.draft` — any string-valued attribute.
    Str { name: String, value: Expr, pos: Pos },
    /// `onClick Reverse`, `onChange Toggle(t.id)`, `onSubmit Add`.
    On {
        event: String,
        handler: MsgCall,
        prevent_default: bool,
    },
    /// `onInput Draft`, `onCheck SetFlag` — the message constructor receives
    /// the event payload.
    OnValue {
        event: String,
        ctor: String,
        kind: ValueKind,
        pos: Pos,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Child {
    Elem(Element),
    Text(Expr, Pos),
    /// `for <var> in <list> { <element> }` — one child per list item.
    For {
        var: String,
        list: Expr,
        body: Box<Element>,
        pos: Pos,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<Attr>,
    pub children: Vec<Child>,
}

/// A `{ field = expr, ... }` record body (used by `init` and `update`).
pub type Record = Vec<(String, Expr, Pos)>;

#[derive(Debug, Clone, PartialEq)]
pub struct RecordDef {
    pub name: String,
    pub fields: Vec<(String, Ty, Pos)>,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MsgDef {
    pub name: String,
    pub payload: Option<Ty>,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    pub msg: String,
    /// The bound payload variable, when the message carries one.
    pub var: Option<(String, Pos)>,
    pub fields: Record,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub name: String,
    pub records: Vec<RecordDef>,
    pub model: Vec<(String, Ty, Pos)>,
    pub init: Record,
    pub msgs: Vec<MsgDef>,
    pub updates: Vec<Update>,
    pub view: Element,
}

/// The reserved record name for the `model {}` block.
pub const MODEL: &str = "Model";

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
