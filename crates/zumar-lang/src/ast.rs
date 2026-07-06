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
    /// `Maybe T` — an optional, backed by Rust `Option<T>`.
    Maybe(Box<Ty>),
    /// A user-declared sum type (`enum Status = Todo | Doing | Done`).
    Enum(String),
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
            Ty::Maybe(t) => write!(f, "Maybe {t}"),
            Ty::Enum(n) => f.write_str(n),
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
    /// `sum(<List Int>)` -> Int
    Sum(Box<Expr>, Pos),
    /// `toInt(<String>)` -> Int (0 when the string isn't an integer)
    ToInt(Box<Expr>, Pos),
    /// `nth(<List T>, <Int>, <T default>)` -> T (default when out of bounds)
    Nth(Box<Expr>, Box<Expr>, Box<Expr>, Pos),
    /// `head(<List T>)` -> Maybe T (none when empty)
    Head(Box<Expr>, Pos),
    /// `none` -> Maybe T (type from context)
    None(Pos),
    /// `some(<T>)` -> Maybe T
    Some(Box<Expr>, Pos),
    /// `Variant(arg)` — an enum constructor with a payload. Bare variants
    /// parse as `Var` and resolve during checking.
    Ctor(String, Box<Expr>, Pos),
    /// `case scrut of <ctor> [binder] -> e | ...` — total: a Maybe needs
    /// both `none` and `some x`; an enum needs every variant.
    Case {
        scrut: Box<Expr>,
        arms: Vec<CaseArm>,
        pos: Pos,
    },
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
    /// `fold(<list>, <init>, <acc> <item> -> <body>)` — the lambda is
    /// syntactic and always applied, so no first-class functions exist.
    Fold {
        list: Box<Expr>,
        init: Box<Expr>,
        acc: String,
        item: String,
        body: Box<Expr>,
        pos: Pos,
    },
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
    /// Integer division; division by zero yields 0 (Elm's rule).
    Div,
    /// Remainder; by zero yields 0.
    Rem,
    Concat,
    Eq,
    Ne,
    Lt,
    Gt,
}

/// A command requested by `init` or an `update` arm (`... then cmd, cmd`).
#[derive(Debug, Clone, PartialEq)]
pub enum CmdCall {
    /// `delay(ms, Msg)` — Msg must be payload-less.
    Delay { ms: i64, msg: String, pos: Pos },
    /// `httpGet(url, Ctor)` — Ctor takes a String: the body on success,
    /// `"error <status>"` on failure.
    HttpGet { url: Expr, ctor: String, pos: Pos },
}

/// `every(ms, Msg)` — Msg payload-less, or `Msg Int` to receive the
/// shim's clock (ms since epoch).
#[derive(Debug, Clone, PartialEq)]
pub struct SubCall {
    pub ms: i64,
    pub msg: String,
    pub pos: Pos,
}

/// The `sub = ...` declaration: a list of subscriptions, possibly chosen
/// by model state. Recomputed after every update; the runtime diffs it.
#[derive(Debug, Clone, PartialEq)]
pub enum SubExpr {
    List(Vec<SubCall>),
    If(Expr, Box<SubExpr>, Box<SubExpr>, Pos),
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

/// One arm of a `case`: constructor, optional payload binder, body.
#[derive(Debug, Clone, PartialEq)]
pub struct CaseArm {
    pub ctor: String,
    pub binder: Option<String>,
    pub body: Expr,
    pub pos: Pos,
}

/// `enum Name = A | B <ty> | C` — variants share one global constructor
/// namespace (Elm-style), each carrying at most one payload.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<(String, Option<Ty>, Pos)>,
    pub pos: Pos,
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
    /// Commands requested alongside this update (`... then cmd, cmd`).
    pub cmds: Vec<CmdCall>,
    pub pos: Pos,
}

#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub name: String,
    pub records: Vec<RecordDef>,
    pub enums: Vec<EnumDef>,
    pub model: Vec<(String, Ty, Pos)>,
    pub init: Record,
    /// Commands fired right after the first render (`init = {...} then cmd`).
    pub init_cmds: Vec<CmdCall>,
    pub msgs: Vec<MsgDef>,
    pub updates: Vec<Update>,
    /// The `sub = ...` declaration, when present.
    pub subs: Option<SubExpr>,
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
