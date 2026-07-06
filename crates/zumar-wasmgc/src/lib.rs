#![forbid(unsafe_code)]
//! WasmGC backend: emit a self-contained GC module from the zumar-lang AST
//! via `wasm-encoder`. No Rust toolchain, no wasm-bindgen, no runtime crate —
//! the module is the whole app.
//!
//! Architecture: `.zu` views are statically known, so the compiler emits a
//! **compile-time patch plan** instead of shipping a diff. Dynamic text and
//! attributes have compile-time paths (SetText/SetAttr); a `for` over a list
//! becomes a **region** — its parent element re-serializes wholesale into a
//! Replace patch on each update. Event handlers inside a region are
//! **parameterized**: the clicked item's index is read from the event path,
//! the loop variable binds to `items[k]`, and the message argument (e.g.
//! `Toggle(t.id)`) is evaluated at dispatch time. Still no vdom, no diff.
//!
//! Data lives on the GC heap: records are structs (Int → i64, Bool → i32,
//! String → `array (mut i8)`), lists of records are `array (mut (ref null
//! $Rec))`. Comprehensions lower to count+fill loops; `++`/`reverse` get
//! per-type emitted helpers; `length(for … where c yield …)` compiles to
//! just the counting loop.
//!
//! Boundary (raw exports, no glue; `www/zumar-gc.js` adapts to the shim):
//! - `init() -> len` — wire-encoded InitialRender at mem[0..len]
//! - `dispatch(event_idx, path_len, payload_len) -> len`
//! - `mem`, `path_buf`, `payload_buf` exports.
//!
//! Not yet here (errors point back to the default Rust backend): `Maybe`,
//! Bool payloads, nested `for`, comprehensions over non-record lists,
//! effects. `key` attributes are accepted and ignored — regions replace
//! wholesale, so keys have nothing to key.

use std::collections::BTreeMap;

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataCountSection, DataSection, ExportKind, ExportSection,
    FieldType, Function, FunctionSection, GlobalSection, GlobalType, HeapType, Instruction as I,
    MemArg, MemorySection, MemoryType, Module, RefType, StartSection, StorageType, TypeSection,
    ValType,
};
use zumar_lang::ast::{App, Attr, Child, Element, Expr, Op, Pos, Ty, ValueKind, MODEL};

const WIRE_VERSION: u8 = 1;

// Memory layout: wire buffer grows from 0; itoa scratch; string constants
// (active data segment); payload and path staging written by the host.
const SCRATCH: i32 = 3072;
const DATA_BASE: i32 = 4096;
const PAYLOAD_BUF: i32 = 56000;
const PATH_BUF: i32 = 60000;

// Fixed function indices (the emitted stdlib comes first).
const F_W8: u32 = 0;
const F_WVU: u32 = 1;
const F_WSTR: u32 = 2;
const F_ITOA: u32 = 3;
const F_MEM2STR: u32 = 4;
const F_CONCAT_STR: u32 = 5;
const F_STREQ: u32 = 6;
const F_WSTRGC: u32 = 7;
const F_FIXED: u32 = 8;

// Globals.
const G_STATE: u32 = 0;
const G_CURSOR: u32 = 1;
const G_PATH_BUF: u32 = 2;
const G_PAYLOAD_BUF: u32 = 3;
const G_PAYLOAD: u32 = 4;
const G_PAYLOAD_I64: u32 = 5;

// Type index for $str is always first.
const T_STR: u32 = 0;

pub fn emit(app: &App) -> Result<Vec<u8>, String> {
    Emitter::build(app)?.module()
}

fn unsupported(pos: Pos, what: &str) -> String {
    format!(
        "{}:{}: {what} is not yet in the wasmgc backend (use the default Rust backend)",
        pos.line, pos.col
    )
}

fn nullable(idx: u32) -> RefType {
    RefType {
        nullable: true,
        heap_type: HeapType::Concrete(idx),
    }
}

fn str_val() -> ValType {
    ValType::Ref(nullable(T_STR))
}

struct Pool {
    bytes: Vec<u8>,
    map: BTreeMap<String, (i32, i32)>,
}

impl Pool {
    /// Returns (segment-relative offset, len). Linear-memory address is
    /// `DATA_BASE + offset`; `array.new_data` uses the offset directly.
    fn intern(&mut self, s: &str) -> (i32, i32) {
        if let Some(&hit) = self.map.get(s) {
            return hit;
        }
        let entry = (self.bytes.len() as i32, s.len() as i32);
        self.bytes.extend_from_slice(s.as_bytes());
        self.map.insert(s.to_string(), entry);
        entry
    }
}

/// Where a name in scope lives during codegen.
#[derive(Clone)]
enum Slot {
    PayloadStr,
    PayloadInt,
    /// A loop variable: local index + its record's name.
    Item(u32, String),
}

/// Per-function codegen context: scope + temp-local allocation.
struct Ctx {
    env: Vec<(String, Slot)>,
    locals: Vec<ValType>,
    base: u32,
}

impl Ctx {
    fn new(base: u32) -> Ctx {
        Ctx {
            env: Vec::new(),
            locals: Vec::new(),
            base,
        }
    }

    fn tmp(&mut self, t: ValType) -> u32 {
        self.locals.push(t);
        self.base + self.locals.len() as u32 - 1
    }

    fn lookup(&self, name: &str) -> Option<Slot> {
        self.env
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s.clone())
    }

    fn into_function(self, fixed: &[ValType], ins: &[I<'static>]) -> Function {
        let mut locals: Vec<(u32, ValType)> = fixed.iter().map(|t| (1, *t)).collect();
        locals.extend(self.locals.iter().map(|t| (1u32, *t)));
        let mut f = Function::new(locals);
        for i in ins {
            f.instruction(i);
        }
        f
    }
}

enum HandlerKind {
    /// Fixed path; String payload comes from the event when `takes_input`.
    Static {
        takes_input: bool,
        arg: Option<Expr>,
    },
    /// Inside a `for` region: prefix + item-index wildcard + suffix.
    ForItem {
        region: usize,
        suffix: Vec<u32>,
        arg: Option<Expr>,
        takes_input: bool,
    },
}

struct Handler {
    prefix: Vec<u32>,
    event: String,
    msg: u32,
    kind: HandlerKind,
}

impl Handler {
    fn specificity(&self) -> usize {
        match &self.kind {
            HandlerKind::Static { .. } => self.prefix.len(),
            HandlerKind::ForItem { suffix, .. } => self.prefix.len() + 1 + suffix.len(),
        }
    }
}

struct DynText {
    path: Vec<u32>,
    expr: Expr,
}

struct DynAttr {
    path: Vec<u32>,
    name: String,
    expr: Expr,
}

/// A `for` region: an element whose children are generated from a list.
struct Region {
    path: Vec<u32>,
    parent_tag: String,
    parent_attrs: Vec<(String, Expr)>,
    var: String,
    list: Expr,
    record: String,
    body: Element,
}

struct Emitter<'a> {
    app: &'a App,
    records: Vec<(String, Vec<(String, Ty)>)>,
    fields: Vec<(String, Ty)>,
    msgs: Vec<String>,
    pool: Pool,
    tree: Vec<I<'static>>,
    dyn_texts: Vec<DynText>,
    dyn_attrs: Vec<DynAttr>,
    regions: Vec<Region>,
    handlers: Vec<Handler>,
    events: BTreeMap<String, bool>,
    /// Temp locals allocated while emitting the init tree.
    init_ctx_locals: Vec<ValType>,
}

impl<'a> Emitter<'a> {
    fn build(app: &'a App) -> Result<Emitter<'a>, String> {
        let mut records = Vec::new();
        for r in &app.records {
            for (name, ty, pos) in &r.fields {
                if !matches!(ty, Ty::Int | Ty::Str | Ty::Bool) {
                    return Err(unsupported(*pos, &format!("record field `{name}: {ty}`")));
                }
            }
            records.push((
                r.name.clone(),
                r.fields
                    .iter()
                    .map(|(n, t, _)| (n.clone(), t.clone()))
                    .collect(),
            ));
        }
        for (name, ty, pos) in &app.model {
            match ty {
                Ty::Int | Ty::Str => {}
                Ty::List(inner) if matches!(inner.as_ref(), Ty::Record(_)) => {}
                _ => return Err(unsupported(*pos, &format!("model field `{name}: {ty}`"))),
            }
        }
        for m in &app.msgs {
            match &m.payload {
                None | Some(Ty::Str) | Some(Ty::Int) => {}
                Some(other) => {
                    return Err(unsupported(m.pos, &format!("`{other}` message payloads")))
                }
            }
        }
        if let Some(cmd) = app
            .init_cmds
            .iter()
            .chain(app.updates.iter().flat_map(|u| u.cmds.iter()))
            .next()
        {
            let pos = match cmd {
                zumar_lang::ast::CmdCall::Delay { pos, .. } => *pos,
                zumar_lang::ast::CmdCall::HttpGet { pos, .. } => *pos,
            };
            return Err(unsupported(pos, "effects (`then` commands)"));
        }
        if app.subs.is_some() {
            return Err(unsupported(
                Pos { line: 1, col: 1 },
                "effects (`sub` subscriptions)",
            ));
        }
        let mut e = Emitter {
            app,
            records,
            fields: app
                .model
                .iter()
                .map(|(n, t, _)| (n.clone(), t.clone()))
                .collect(),
            msgs: app.msgs.iter().map(|m| m.name.clone()).collect(),
            pool: Pool {
                bytes: Vec::new(),
                map: BTreeMap::new(),
            },
            tree: Vec::new(),
            dyn_texts: Vec::new(),
            dyn_attrs: Vec::new(),
            regions: Vec::new(),
            handlers: Vec::new(),
            events: BTreeMap::new(),
            init_ctx_locals: Vec::new(),
        };
        // The init tree needs a Ctx for inline dynamic expressions.
        let mut ctx = Ctx::new(0);
        let mut tree = Vec::new();
        let mut path = Vec::new();
        e.walk(&app.view, &mut path, &mut ctx, &mut tree)?;
        e.tree = tree;
        e.init_ctx_locals = ctx.locals.clone();
        // Deepest-first so bubbling picks the innermost handler.
        e.handlers
            .sort_by_key(|h| std::cmp::Reverse(h.specificity()));
        Ok(e)
    }

    // --- type & function index layout ---------------------------------------

    fn n_records(&self) -> u32 {
        self.records.len() as u32
    }

    fn rec_idx(&self, name: &str) -> u32 {
        1 + self
            .records
            .iter()
            .position(|(n, _)| n == name)
            .expect("typechecked") as u32
    }

    fn arr_idx(&self, name: &str) -> u32 {
        1 + self.n_records()
            + self
                .records
                .iter()
                .position(|(n, _)| n == name)
                .expect("typechecked") as u32
    }

    fn model_idx(&self) -> u32 {
        1 + 2 * self.n_records()
    }

    fn ft(&self, k: u32) -> u32 {
        2 + 2 * self.n_records() + k
    }

    // Function-type slots (after the data types).
    fn t_i32(&self) -> u32 {
        self.ft(0)
    }
    fn t_ii(&self) -> u32 {
        self.ft(1)
    }
    fn t_void(&self) -> u32 {
        self.ft(2)
    }
    fn t_ret(&self) -> u32 {
        self.ft(3)
    }
    fn t_dispatch(&self) -> u32 {
        self.ft(4)
    }
    fn t_itoa(&self) -> u32 {
        self.ft(5)
    }
    fn t_mem2str(&self) -> u32 {
        self.ft(6)
    }
    fn t_concat_str(&self) -> u32 {
        self.ft(7)
    }
    fn t_streq(&self) -> u32 {
        self.ft(8)
    }
    fn t_wstrgc(&self) -> u32 {
        self.ft(9)
    }
    fn t_arrcat(&self, rec: usize) -> u32 {
        self.ft(10 + 2 * rec as u32)
    }
    fn t_arrrev(&self, rec: usize) -> u32 {
        self.ft(10 + 2 * rec as u32 + 1)
    }

    fn f_arrcat(&self, name: &str) -> u32 {
        F_FIXED + 2 * self.records.iter().position(|(n, _)| n == name).unwrap() as u32
    }
    fn f_arrrev(&self, name: &str) -> u32 {
        self.f_arrcat(name) + 1
    }
    fn f_region(&self, j: usize) -> u32 {
        F_FIXED + 2 * self.n_records() + j as u32
    }
    fn f_update(&self) -> u32 {
        self.f_region(self.regions.len())
    }
    fn f_patches(&self) -> u32 {
        self.f_update() + 1
    }
    fn f_boot(&self) -> u32 {
        self.f_update() + 2
    }
    fn f_init(&self) -> u32 {
        self.f_update() + 3
    }
    fn f_dispatch(&self) -> u32 {
        self.f_update() + 4
    }

    fn field_index(&self, name: &str) -> u32 {
        self.fields
            .iter()
            .position(|(f, _)| f == name)
            .expect("typechecked") as u32
    }

    fn field_ty(&self, name: &str) -> Ty {
        self.fields
            .iter()
            .find(|(f, _)| f == name)
            .map(|(_, t)| t.clone())
            .expect("typechecked")
    }

    fn record_fields(&self, name: &str) -> &[(String, Ty)] {
        &self
            .records
            .iter()
            .find(|(n, _)| n == name)
            .expect("typechecked")
            .1
    }

    fn record_field_index(&self, rec: &str, field: &str) -> u32 {
        self.record_fields(rec)
            .iter()
            .position(|(f, _)| f == field)
            .expect("typechecked") as u32
    }

    fn record_field_ty(&self, rec: &str, field: &str) -> Ty {
        self.record_fields(rec)
            .iter()
            .find(|(f, _)| f == field)
            .map(|(_, t)| t.clone())
            .expect("typechecked")
    }

    fn msg_index(&self, name: &str) -> u32 {
        self.msgs
            .iter()
            .position(|m| m == name)
            .expect("typechecked") as u32
    }

    fn msg_payload(&self, index: u32) -> Option<Ty> {
        self.app.msgs[index as usize].payload.clone()
    }

    fn event_code(&self, name: &str) -> i32 {
        self.events
            .keys()
            .position(|e| e == name)
            .expect("collected") as i32
    }

    fn val_type(&self, ty: &Ty) -> ValType {
        match ty {
            Ty::Int => ValType::I64,
            Ty::Bool => ValType::I32,
            Ty::Str => str_val(),
            Ty::Record(r) => ValType::Ref(nullable(self.rec_idx(r))),
            Ty::List(inner) => {
                let Ty::Record(r) = inner.as_ref() else {
                    unreachable!("gated: record lists only")
                };
                ValType::Ref(nullable(self.arr_idx(r)))
            }
            Ty::Maybe(_) => unreachable!("gated"),
        }
    }

    /// Type resolution for the supported subset (checker-validated programs;
    /// this only steers codegen).
    fn ty_of(&self, e: &Expr, ctx: &Ctx) -> Ty {
        match e {
            Expr::Int(_) | Expr::ToInt(..) | Expr::Len(..) | Expr::Sum(..) => Ty::Int,
            Expr::Str(_) | Expr::Show(..) => Ty::Str,
            Expr::Bool(_) | Expr::Not(..) => Ty::Bool,
            Expr::Var(v, _) if v == "model" => Ty::Record(MODEL.into()),
            Expr::Var(v, _) => match ctx.lookup(v) {
                Some(Slot::PayloadStr) => Ty::Str,
                Some(Slot::PayloadInt) => Ty::Int,
                Some(Slot::Item(_, r)) => Ty::Record(r),
                None => Ty::Int,
            },
            Expr::Field(base, f, _) => match self.ty_of(base, ctx) {
                Ty::Record(r) if r == MODEL => self.field_ty(f),
                Ty::Record(r) => self.record_field_ty(&r, f),
                _ => Ty::Int,
            },
            Expr::Bin(Op::Concat, l, ..) => self.ty_of(l, ctx),
            Expr::Bin(Op::Add | Op::Sub | Op::Mul | Op::Div | Op::Rem, ..) => Ty::Int,
            Expr::Bin(..) => Ty::Bool,
            Expr::If(_, t, ..) => self.ty_of(t, ctx),
            Expr::Reverse(inner, _) => self.ty_of(inner, ctx),
            Expr::For {
                var, list, body, ..
            } => {
                let Ty::List(elem) = self.ty_of(list, ctx) else {
                    return Ty::Int;
                };
                let mut inner = Ctx::new(0);
                inner.env = ctx.env.clone();
                if let Ty::Record(r) = elem.as_ref() {
                    inner.env.push((var.clone(), Slot::Item(0, r.clone())));
                }
                Ty::List(Box::new(self.ty_of(body, &inner)))
            }
            Expr::ListLit(items, _) => match items.first() {
                Some(first) => Ty::List(Box::new(self.ty_of(first, ctx))),
                None => Ty::List(Box::new(Ty::Int)),
            },
            Expr::RecordLit(fields, _) => {
                let got: Vec<&str> = fields.iter().map(|(n, _, _)| n.as_str()).collect();
                let name = self
                    .records
                    .iter()
                    .find(|(_, defs)| {
                        defs.len() == got.len()
                            && defs.iter().all(|(n, _)| got.contains(&n.as_str()))
                    })
                    .map(|(n, _)| n.clone())
                    .unwrap_or_default();
                Ty::Record(name)
            }
            Expr::RecordUpdate(base, ..) => self.ty_of(base, ctx),
            _ => Ty::Int,
        }
    }

    // --- write helpers ------------------------------------------------------

    fn c_w8(out: &mut Vec<I<'static>>, byte: u8) {
        out.push(I::I32Const(byte as i32));
        out.push(I::Call(F_W8));
    }

    fn c_wvu(out: &mut Vec<I<'static>>, n: u32) {
        out.push(I::I32Const(n as i32));
        out.push(I::Call(F_WVU));
    }

    fn c_wstr(&mut self, out: &mut Vec<I<'static>>, s: &str) {
        let (off, len) = self.pool.intern(s);
        out.push(I::I32Const(DATA_BASE + off));
        out.push(I::I32Const(len));
        out.push(I::Call(F_WSTR));
    }

    /// Write a text expression: constants stream from the pool, dynamic
    /// expressions build a GC string and serialize it.
    fn c_text(
        &mut self,
        expr: &Expr,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        if let Expr::Str(s) = expr {
            let s = s.clone();
            self.c_wstr(out, &s);
            return Ok(());
        }
        self.str_expr(expr, ctx, out)?;
        out.push(I::Call(F_WSTRGC));
        Ok(())
    }

    // --- view walk (static part) ---------------------------------------------

    fn walk(
        &mut self,
        el: &Element,
        path: &mut Vec<u32>,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        // An element whose children come from a `for` becomes a region.
        let fors = el
            .children
            .iter()
            .filter(|c| matches!(c, Child::For { .. }))
            .count();
        if fors > 0 {
            if el.children.len() != 1 {
                let Child::For { pos, .. } = el
                    .children
                    .iter()
                    .find(|c| matches!(c, Child::For { .. }))
                    .unwrap()
                else {
                    unreachable!()
                };
                return Err(unsupported(
                    *pos,
                    "`for` next to sibling children (make the `for` the element's only child)",
                ));
            }
            let Child::For {
                var,
                list,
                body,
                pos,
            } = &el.children[0]
            else {
                unreachable!()
            };
            let Ty::List(elem) = self.ty_of(list, ctx) else {
                return Err(unsupported(*pos, "this list source"));
            };
            let Ty::Record(record) = elem.as_ref() else {
                return Err(unsupported(*pos, "rendering a non-record list"));
            };
            let j = self.regions.len();
            let parent_attrs = self.plain_attrs(el, path)?;
            self.regions.push(Region {
                path: path.clone(),
                parent_tag: el.tag.clone(),
                parent_attrs,
                var: var.clone(),
                list: list.clone(),
                record: record.clone(),
                body: (**body).clone(),
            });
            // Handlers inside the body are collected now, with suffix paths.
            let body = (**body).clone();
            let var = var.clone();
            let record = record.clone();
            let mut suffix = Vec::new();
            self.collect_body_handlers(&body, path, &mut suffix, j, &var, &record)?;
            out.push(I::Call(self.f_region(j)));
            return Ok(());
        }

        Self::c_w8(out, 1); // element
        let tag = el.tag.clone();
        self.c_wstr(out, &tag);

        let attrs = self.plain_attrs(el, path)?;
        Self::c_wvu(out, attrs.len() as u32);
        for (name, value) in &attrs {
            let name = name.clone();
            let value = value.clone();
            self.c_wstr(out, &name);
            if !matches!(value, Expr::Str(_)) {
                self.dyn_attrs.push(DynAttr {
                    path: path.clone(),
                    name,
                    expr: value.clone(),
                });
            }
            self.c_text(&value, ctx, out)?;
        }

        Self::c_wvu(out, el.children.len() as u32);
        for (i, child) in el.children.iter().enumerate() {
            path.push(i as u32);
            match child {
                Child::Elem(e) => self.walk(e, path, ctx, out)?,
                Child::Text(expr, _) => {
                    Self::c_w8(out, 0); // text node
                    if !matches!(expr, Expr::Str(_)) {
                        self.dyn_texts.push(DynText {
                            path: path.clone(),
                            expr: expr.clone(),
                        });
                    }
                    let expr = expr.clone();
                    self.c_text(&expr, ctx, out)?;
                }
                Child::For { .. } => unreachable!("handled above"),
            }
            path.pop();
        }
        Ok(())
    }

    /// Split an element's attributes: string attrs returned (key skipped),
    /// event handlers registered as static handlers at `path`.
    fn plain_attrs(&mut self, el: &Element, path: &[u32]) -> Result<Vec<(String, Expr)>, String> {
        let mut plain = Vec::new();
        for attr in &el.attrs {
            match attr {
                Attr::Str { name, value, .. } => {
                    if name == "key" {
                        continue; // regions replace wholesale; keys are inert
                    }
                    plain.push((name.clone(), value.clone()));
                }
                Attr::On {
                    event,
                    handler,
                    prevent_default,
                } => {
                    let msg = self.msg_index(&handler.name);
                    let pd = self.events.entry(event.clone()).or_insert(false);
                    *pd = *pd || *prevent_default;
                    self.handlers.push(Handler {
                        prefix: path.to_vec(),
                        event: event.clone(),
                        msg,
                        kind: HandlerKind::Static {
                            takes_input: false,
                            arg: handler.arg.clone(),
                        },
                    });
                }
                Attr::OnValue {
                    event,
                    ctor,
                    kind,
                    pos,
                } => {
                    if *kind == ValueKind::Checked {
                        return Err(unsupported(*pos, "`onCheck` (Bool payloads)"));
                    }
                    let msg = self.msg_index(ctor);
                    self.events.entry(event.clone()).or_insert(false);
                    self.handlers.push(Handler {
                        prefix: path.to_vec(),
                        event: event.clone(),
                        msg,
                        kind: HandlerKind::Static {
                            takes_input: true,
                            arg: None,
                        },
                    });
                }
            }
        }
        Ok(plain)
    }

    /// Register handlers found inside a region body (suffix-addressed).
    fn collect_body_handlers(
        &mut self,
        el: &Element,
        prefix: &[u32],
        suffix: &mut Vec<u32>,
        region: usize,
        var: &str,
        record: &str,
    ) -> Result<(), String> {
        for attr in &el.attrs {
            match attr {
                Attr::Str { .. } => {}
                Attr::On {
                    event,
                    handler,
                    prevent_default,
                } => {
                    let msg = self.msg_index(&handler.name);
                    let pd = self.events.entry(event.clone()).or_insert(false);
                    *pd = *pd || *prevent_default;
                    self.handlers.push(Handler {
                        prefix: prefix.to_vec(),
                        event: event.clone(),
                        msg,
                        kind: HandlerKind::ForItem {
                            region,
                            suffix: suffix.clone(),
                            arg: handler.arg.clone(),
                            takes_input: false,
                        },
                    });
                }
                Attr::OnValue {
                    event,
                    ctor,
                    kind,
                    pos,
                } => {
                    if *kind == ValueKind::Checked {
                        return Err(unsupported(*pos, "`onCheck` (Bool payloads)"));
                    }
                    let msg = self.msg_index(ctor);
                    self.events.entry(event.clone()).or_insert(false);
                    self.handlers.push(Handler {
                        prefix: prefix.to_vec(),
                        event: event.clone(),
                        msg,
                        kind: HandlerKind::ForItem {
                            region,
                            suffix: suffix.clone(),
                            arg: None,
                            takes_input: true,
                        },
                    });
                }
            }
        }
        for (i, child) in el.children.iter().enumerate() {
            suffix.push(i as u32);
            match child {
                Child::Elem(e) => {
                    self.collect_body_handlers(e, prefix, suffix, region, var, record)?
                }
                Child::Text(..) => {}
                Child::For { pos, .. } => {
                    return Err(unsupported(*pos, "nested `for` regions"));
                }
            }
            suffix.pop();
        }
        let _ = (var, record);
        Ok(())
    }

    // --- expressions ----------------------------------------------------------

    /// Push an i64.
    fn int_expr(
        &mut self,
        e: &Expr,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        match e {
            Expr::Int(n) => out.push(I::I64Const(*n)),
            Expr::Var(v, pos) => match ctx.lookup(v) {
                Some(Slot::PayloadInt) => out.push(I::GlobalGet(G_PAYLOAD_I64)),
                _ => return Err(unsupported(*pos, &format!("variable `{v}` here"))),
            },
            Expr::Field(..) => self.place(e, ctx, out)?,
            Expr::Len(inner, _) => match inner.as_ref() {
                // length(for … where c yield …) compiles to just the count.
                Expr::For {
                    var, list, cond, ..
                } => {
                    self.count_loop(var, list, cond.as_deref(), ctx, out)?;
                    out.push(I::I64ExtendI32U);
                }
                _ => {
                    self.list_expr(inner, ctx, out)?;
                    out.push(I::ArrayLen);
                    out.push(I::I64ExtendI32U);
                }
            },
            Expr::Bin(op @ (Op::Add | Op::Sub | Op::Mul), l, r, _) => {
                self.int_expr(l, ctx, out)?;
                self.int_expr(r, ctx, out)?;
                out.push(match op {
                    Op::Add => I::I64Add,
                    Op::Sub => I::I64Sub,
                    _ => I::I64Mul,
                });
            }
            // Division/remainder by zero yield 0 (Elm's rule): guard the trap.
            Expr::Bin(op @ (Op::Div | Op::Rem), l, r, _) => {
                let a = ctx.tmp(ValType::I64);
                let b = ctx.tmp(ValType::I64);
                self.int_expr(l, ctx, out)?;
                out.push(I::LocalSet(a));
                self.int_expr(r, ctx, out)?;
                out.push(I::LocalSet(b));
                out.push(I::LocalGet(b));
                out.push(I::I64Eqz);
                out.push(I::If(BlockType::Result(ValType::I64)));
                out.push(I::I64Const(0));
                out.push(I::Else);
                out.push(I::LocalGet(a));
                out.push(I::LocalGet(b));
                out.push(if *op == Op::Div {
                    I::I64DivS
                } else {
                    I::I64RemS
                });
                out.push(I::End);
            }
            Expr::If(c, t, f, _) => {
                self.bool_expr(c, ctx, out)?;
                out.push(I::If(BlockType::Result(ValType::I64)));
                self.int_expr(t, ctx, out)?;
                out.push(I::Else);
                self.int_expr(f, ctx, out)?;
                out.push(I::End);
            }
            other => return Err(unsupported(pos_of(other), "this expression")),
        }
        Ok(())
    }

    /// Push a `(ref null $str)`.
    fn str_expr(
        &mut self,
        e: &Expr,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        match e {
            Expr::Str(s) => {
                let (off, len) = self.pool.intern(s);
                out.push(I::I32Const(off));
                out.push(I::I32Const(len));
                out.push(I::ArrayNewData {
                    array_type_index: T_STR,
                    array_data_index: 1,
                });
            }
            Expr::Var(v, pos) => match ctx.lookup(v) {
                Some(Slot::PayloadStr) => out.push(I::GlobalGet(G_PAYLOAD)),
                _ => return Err(unsupported(*pos, &format!("variable `{v}` here"))),
            },
            Expr::Field(..) => self.place(e, ctx, out)?,
            Expr::Show(inner, _) => {
                self.int_expr(inner, ctx, out)?;
                out.push(I::Call(F_ITOA));
            }
            Expr::Bin(Op::Concat, l, r, _) => {
                self.str_expr(l, ctx, out)?;
                self.str_expr(r, ctx, out)?;
                out.push(I::Call(F_CONCAT_STR));
            }
            Expr::If(c, t, f, _) => {
                self.bool_expr(c, ctx, out)?;
                out.push(I::If(BlockType::Result(str_val())));
                self.str_expr(t, ctx, out)?;
                out.push(I::Else);
                self.str_expr(f, ctx, out)?;
                out.push(I::End);
            }
            other => return Err(unsupported(pos_of(other), "this text expression")),
        }
        Ok(())
    }

    /// Push an i32 boolean.
    fn bool_expr(
        &mut self,
        e: &Expr,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        match e {
            Expr::Bool(b) => out.push(I::I32Const(*b as i32)),
            Expr::Not(inner, _) => {
                self.bool_expr(inner, ctx, out)?;
                out.push(I::I32Eqz);
            }
            Expr::Field(..) => self.place(e, ctx, out)?, // Bool struct field (i32)
            Expr::Bin(op @ (Op::Eq | Op::Ne), l, r, _) => {
                if self.ty_of(l, ctx) == Ty::Str {
                    self.str_expr(l, ctx, out)?;
                    self.str_expr(r, ctx, out)?;
                    out.push(I::Call(F_STREQ));
                    if *op == Op::Ne {
                        out.push(I::I32Eqz);
                    }
                } else {
                    self.int_expr(l, ctx, out)?;
                    self.int_expr(r, ctx, out)?;
                    out.push(if *op == Op::Eq { I::I64Eq } else { I::I64Ne });
                }
            }
            Expr::Bin(op @ (Op::Lt | Op::Gt), l, r, _) => {
                self.int_expr(l, ctx, out)?;
                self.int_expr(r, ctx, out)?;
                out.push(if *op == Op::Lt { I::I64LtS } else { I::I64GtS });
            }
            other => return Err(unsupported(pos_of(other), "this condition")),
        }
        Ok(())
    }

    /// `model.<f>` or `<loopvar>.<f>` -> struct.get (any field type).
    fn place(&mut self, e: &Expr, ctx: &mut Ctx, out: &mut Vec<I<'static>>) -> Result<(), String> {
        let Expr::Field(base, field, pos) = e else {
            return Err(unsupported(pos_of(e), "this access"));
        };
        let Expr::Var(v, _) = base.as_ref() else {
            return Err(unsupported(*pos, "nested field access"));
        };
        if v == "model" {
            out.push(I::GlobalGet(G_STATE));
            out.push(I::StructGet {
                struct_type_index: self.model_idx(),
                field_index: self.field_index(field),
            });
            return Ok(());
        }
        match ctx.lookup(v) {
            Some(Slot::Item(local, rec)) => {
                out.push(I::LocalGet(local));
                out.push(I::StructGet {
                    struct_type_index: self.rec_idx(&rec),
                    field_index: self.record_field_index(&rec, field),
                });
                Ok(())
            }
            _ => Err(unsupported(*pos, &format!("variable `{v}`"))),
        }
    }

    /// Push a `(ref null $Rec)`.
    fn rec_expr(
        &mut self,
        e: &Expr,
        rec: &str,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        match e {
            Expr::Var(v, pos) => match ctx.lookup(v) {
                Some(Slot::Item(local, _)) => out.push(I::LocalGet(local)),
                _ => return Err(unsupported(*pos, &format!("variable `{v}` here"))),
            },
            Expr::RecordLit(fields, _) => {
                for (fname, fty) in self.record_fields(rec).to_vec() {
                    let expr = fields
                        .iter()
                        .find(|(n, _, _)| *n == fname)
                        .map(|(_, e, _)| e.clone())
                        .expect("typechecked: literal is total");
                    self.val_expr(&expr, &fty, ctx, out)?;
                }
                out.push(I::StructNew(self.rec_idx(rec)));
            }
            Expr::RecordUpdate(base, overrides, _) => {
                let rec_ty = ValType::Ref(nullable(self.rec_idx(rec)));
                let base_local = ctx.tmp(rec_ty);
                self.rec_expr(base, rec, ctx, out)?;
                out.push(I::LocalSet(base_local));
                for (i, (fname, fty)) in self.record_fields(rec).to_vec().iter().enumerate() {
                    match overrides.iter().find(|(n, _, _)| n == fname) {
                        Some((_, e, _)) => {
                            let e = e.clone();
                            self.val_expr(&e, fty, ctx, out)?;
                        }
                        None => {
                            out.push(I::LocalGet(base_local));
                            out.push(I::StructGet {
                                struct_type_index: self.rec_idx(rec),
                                field_index: i as u32,
                            });
                        }
                    }
                }
                out.push(I::StructNew(self.rec_idx(rec)));
            }
            Expr::If(c, t, f, _) => {
                self.bool_expr(c, ctx, out)?;
                out.push(I::If(BlockType::Result(ValType::Ref(nullable(
                    self.rec_idx(rec),
                )))));
                self.rec_expr(t, rec, ctx, out)?;
                out.push(I::Else);
                self.rec_expr(f, rec, ctx, out)?;
                out.push(I::End);
            }
            other => return Err(unsupported(pos_of(other), "this record expression")),
        }
        Ok(())
    }

    /// Push a `(ref null $Arr<Rec>)`.
    fn list_expr(
        &mut self,
        e: &Expr,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        match e {
            Expr::Field(..) => self.place(e, ctx, out)?,
            Expr::ListLit(items, pos) => {
                let Ty::List(elem) = self.ty_of(e, ctx) else {
                    return Err(unsupported(*pos, "this list"));
                };
                let Ty::Record(rec) = elem.as_ref() else {
                    return Err(unsupported(*pos, "non-record list literals"));
                };
                // Empty literals get their type from init/update field types
                // upstream — here they only appear record-typed.
                for item in items.clone() {
                    self.rec_expr(&item, rec, ctx, out)?;
                }
                out.push(I::ArrayNewFixed {
                    array_type_index: self.arr_idx(rec),
                    array_size: items.len() as u32,
                });
            }
            Expr::Bin(Op::Concat, l, r, pos) => {
                let Ty::List(elem) = self.ty_of(l, ctx) else {
                    return Err(unsupported(*pos, "this concat"));
                };
                let Ty::Record(rec) = elem.as_ref() else {
                    return Err(unsupported(*pos, "non-record list concat"));
                };
                let rec = rec.clone();
                self.list_expr(l, ctx, out)?;
                self.list_expr(r, ctx, out)?;
                out.push(I::Call(self.f_arrcat(&rec)));
            }
            Expr::Reverse(inner, pos) => {
                let Ty::List(elem) = self.ty_of(inner, ctx) else {
                    return Err(unsupported(*pos, "this reverse"));
                };
                let Ty::Record(rec) = elem.as_ref() else {
                    return Err(unsupported(*pos, "non-record list reverse"));
                };
                let rec = rec.clone();
                self.list_expr(inner, ctx, out)?;
                out.push(I::Call(self.f_arrrev(&rec)));
            }
            Expr::For {
                var,
                list,
                cond,
                body,
                pos,
            } => {
                let Ty::List(elem) = self.ty_of(list, ctx) else {
                    return Err(unsupported(*pos, "this comprehension source"));
                };
                let Ty::Record(rec) = elem.as_ref() else {
                    return Err(unsupported(*pos, "comprehensions over non-record lists"));
                };
                let rec = rec.clone();
                self.comprehension(var, list, cond.as_deref(), body, &rec, ctx, out)?;
            }
            other => return Err(unsupported(pos_of(other), "this list expression")),
        }
        Ok(())
    }

    /// A value of the given type (drives update arms, boot, record fields).
    fn val_expr(
        &mut self,
        e: &Expr,
        ty: &Ty,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        match ty {
            Ty::Int => self.int_expr(e, ctx, out),
            Ty::Str => self.str_expr(e, ctx, out),
            Ty::Bool => self.bool_expr(e, ctx, out),
            Ty::Record(r) => self.rec_expr(e, r, ctx, out),
            Ty::List(elem) => {
                let Ty::Record(rec) = elem.as_ref() else {
                    return Err(unsupported(pos_of(e), "this list type"));
                };
                // Empty literals need the element type from the field.
                if let Expr::ListLit(items, _) = e {
                    if items.is_empty() {
                        out.push(I::ArrayNewFixed {
                            array_type_index: self.arr_idx(rec),
                            array_size: 0,
                        });
                        return Ok(());
                    }
                }
                self.list_expr(e, ctx, out)
            }
            Ty::Maybe(_) => Err(unsupported(pos_of(e), "`Maybe` values")),
        }
    }

    /// The counting half of a comprehension: pushes an i32 count.
    fn count_loop(
        &mut self,
        var: &str,
        list: &Expr,
        cond: Option<&Expr>,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        let Ty::List(elem) = self.ty_of(list, ctx) else {
            return Err(unsupported(pos_of(list), "this comprehension source"));
        };
        let Ty::Record(rec) = elem.as_ref() else {
            return Err(unsupported(
                pos_of(list),
                "comprehensions over non-record lists",
            ));
        };
        let rec = rec.clone();
        let arr_ty = ValType::Ref(nullable(self.arr_idx(&rec)));
        let rec_vt = ValType::Ref(nullable(self.rec_idx(&rec)));
        let src = ctx.tmp(arr_ty);
        let n = ctx.tmp(ValType::I32);
        let i = ctx.tmp(ValType::I32);
        let cnt = ctx.tmp(ValType::I32);
        let t = ctx.tmp(rec_vt);

        self.list_expr(list, ctx, out)?;
        out.push(I::LocalSet(src));
        out.push(I::LocalGet(src));
        out.push(I::ArrayLen);
        out.push(I::LocalSet(n));
        out.push(I::I32Const(0));
        out.push(I::LocalSet(i));
        out.push(I::I32Const(0));
        out.push(I::LocalSet(cnt));
        match cond {
            None => {
                out.push(I::LocalGet(n));
                out.push(I::LocalSet(cnt));
            }
            Some(c) => {
                out.push(I::Block(BlockType::Empty));
                out.push(I::Loop(BlockType::Empty));
                out.push(I::LocalGet(i));
                out.push(I::LocalGet(n));
                out.push(I::I32GeU);
                out.push(I::BrIf(1));
                out.push(I::LocalGet(src));
                out.push(I::LocalGet(i));
                out.push(I::ArrayGet(self.arr_idx(&rec)));
                out.push(I::LocalSet(t));
                ctx.env.push((var.to_string(), Slot::Item(t, rec.clone())));
                self.bool_expr(c, ctx, out)?;
                ctx.env.pop();
                out.push(I::If(BlockType::Empty));
                out.push(I::LocalGet(cnt));
                out.push(I::I32Const(1));
                out.push(I::I32Add);
                out.push(I::LocalSet(cnt));
                out.push(I::End);
                out.push(I::LocalGet(i));
                out.push(I::I32Const(1));
                out.push(I::I32Add);
                out.push(I::LocalSet(i));
                out.push(I::Br(0));
                out.push(I::End);
                out.push(I::End);
            }
        }
        out.push(I::LocalGet(cnt));
        Ok(())
    }

    /// Full comprehension: count pass (when filtered) + fill pass.
    #[allow(clippy::too_many_arguments)]
    fn comprehension(
        &mut self,
        var: &str,
        list: &Expr,
        cond: Option<&Expr>,
        body: &Expr,
        rec: &str,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        let arr_idx = self.arr_idx(rec);
        let arr_ty = ValType::Ref(nullable(arr_idx));
        let rec_vt = ValType::Ref(nullable(self.rec_idx(rec)));
        let src = ctx.tmp(arr_ty);
        let n = ctx.tmp(ValType::I32);
        let i = ctx.tmp(ValType::I32);
        let w = ctx.tmp(ValType::I32);
        let outp = ctx.tmp(arr_ty);
        let t = ctx.tmp(rec_vt);

        self.list_expr(list, ctx, out)?;
        out.push(I::LocalSet(src));
        out.push(I::LocalGet(src));
        out.push(I::ArrayLen);
        out.push(I::LocalSet(n));

        // Size: n when unfiltered, else a counting pass.
        match cond {
            None => out.push(I::LocalGet(n)),
            Some(c) => {
                out.push(I::I32Const(0));
                out.push(I::LocalSet(i));
                out.push(I::I32Const(0));
                out.push(I::LocalSet(w));
                out.push(I::Block(BlockType::Empty));
                out.push(I::Loop(BlockType::Empty));
                out.push(I::LocalGet(i));
                out.push(I::LocalGet(n));
                out.push(I::I32GeU);
                out.push(I::BrIf(1));
                out.push(I::LocalGet(src));
                out.push(I::LocalGet(i));
                out.push(I::ArrayGet(arr_idx));
                out.push(I::LocalSet(t));
                ctx.env
                    .push((var.to_string(), Slot::Item(t, rec.to_string())));
                self.bool_expr(c, ctx, out)?;
                ctx.env.pop();
                out.push(I::If(BlockType::Empty));
                out.push(I::LocalGet(w));
                out.push(I::I32Const(1));
                out.push(I::I32Add);
                out.push(I::LocalSet(w));
                out.push(I::End);
                out.push(I::LocalGet(i));
                out.push(I::I32Const(1));
                out.push(I::I32Add);
                out.push(I::LocalSet(i));
                out.push(I::Br(0));
                out.push(I::End);
                out.push(I::End);
                out.push(I::LocalGet(w));
            }
        }
        out.push(I::ArrayNewDefault(arr_idx));
        out.push(I::LocalSet(outp));

        // Fill pass.
        out.push(I::I32Const(0));
        out.push(I::LocalSet(i));
        out.push(I::I32Const(0));
        out.push(I::LocalSet(w));
        out.push(I::Block(BlockType::Empty));
        out.push(I::Loop(BlockType::Empty));
        out.push(I::LocalGet(i));
        out.push(I::LocalGet(n));
        out.push(I::I32GeU);
        out.push(I::BrIf(1));
        out.push(I::LocalGet(src));
        out.push(I::LocalGet(i));
        out.push(I::ArrayGet(arr_idx));
        out.push(I::LocalSet(t));
        ctx.env
            .push((var.to_string(), Slot::Item(t, rec.to_string())));
        let inner: Result<(), String> = (|| {
            if let Some(c) = cond {
                self.bool_expr(c, ctx, out)?;
                out.push(I::If(BlockType::Empty));
            }
            out.push(I::LocalGet(outp));
            out.push(I::LocalGet(w));
            self.rec_expr(body, rec, ctx, out)?;
            out.push(I::ArraySet(arr_idx));
            out.push(I::LocalGet(w));
            out.push(I::I32Const(1));
            out.push(I::I32Add);
            out.push(I::LocalSet(w));
            if cond.is_some() {
                out.push(I::End);
            }
            Ok(())
        })();
        ctx.env.pop();
        inner?;
        out.push(I::LocalGet(i));
        out.push(I::I32Const(1));
        out.push(I::I32Add);
        out.push(I::LocalSet(i));
        out.push(I::Br(0));
        out.push(I::End);
        out.push(I::End);
        out.push(I::LocalGet(outp));
        Ok(())
    }

    // --- module assembly -------------------------------------------------------

    fn module(mut self) -> Result<Vec<u8>, String> {
        let mut module = Module::new();

        // Types: $str, record structs, record arrays, model, function types.
        let mut types = TypeSection::new();
        types.ty().array(&StorageType::I8, true);
        for (_, fields) in &self.records.clone() {
            types.ty().struct_(
                fields
                    .iter()
                    .map(|(_, t)| FieldType {
                        element_type: StorageType::Val(match t {
                            Ty::Int => ValType::I64,
                            Ty::Bool => ValType::I32,
                            _ => str_val(),
                        }),
                        mutable: true,
                    })
                    .collect::<Vec<_>>(),
            );
        }
        for i in 0..self.records.len() {
            types.ty().array(
                &StorageType::Val(ValType::Ref(nullable(1 + i as u32))),
                true,
            );
        }
        types.ty().struct_(
            self.fields
                .clone()
                .iter()
                .map(|(_, t)| FieldType {
                    element_type: StorageType::Val(self.val_type(t)),
                    mutable: true,
                })
                .collect::<Vec<_>>(),
        );
        types.ty().function([ValType::I32], []);
        types.ty().function([ValType::I32, ValType::I32], []);
        types.ty().function([], []);
        types.ty().function([], [ValType::I32]);
        types
            .ty()
            .function([ValType::I32, ValType::I32, ValType::I32], [ValType::I32]);
        types.ty().function([ValType::I64], [str_val()]);
        types
            .ty()
            .function([ValType::I32, ValType::I32], [str_val()]);
        types.ty().function([str_val(), str_val()], [str_val()]);
        types.ty().function([str_val(), str_val()], [ValType::I32]);
        types.ty().function([str_val()], []);
        for i in 0..self.records.len() {
            let arr = ValType::Ref(nullable(1 + self.n_records() + i as u32));
            types.ty().function([arr, arr], [arr]);
            types.ty().function([arr], [arr]);
        }
        module.section(&types);

        // Functions.
        let mut funcs = FunctionSection::new();
        for ty in [
            self.t_i32(),
            self.t_i32(),
            self.t_ii(),
            self.t_itoa(),
            self.t_mem2str(),
            self.t_concat_str(),
            self.t_streq(),
            self.t_wstrgc(),
        ] {
            funcs.function(ty);
        }
        for i in 0..self.records.len() {
            funcs.function(self.t_arrcat(i));
            funcs.function(self.t_arrrev(i));
        }
        for _ in 0..self.regions.len() {
            funcs.function(self.t_void());
        }
        funcs.function(self.t_i32()); // update
        funcs.function(self.t_void()); // patches
        funcs.function(self.t_void()); // boot
        funcs.function(self.t_ret()); // init
        funcs.function(self.t_dispatch()); // dispatch
        module.section(&funcs);

        // Memory.
        let mut memory = MemorySection::new();
        memory.memory(MemoryType {
            minimum: 2,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        module.section(&memory);

        // Globals.
        let mut globals = GlobalSection::new();
        globals.global(
            GlobalType {
                val_type: ValType::Ref(nullable(self.model_idx())),
                mutable: true,
                shared: false,
            },
            &ConstExpr::ref_null(HeapType::Concrete(self.model_idx())),
        );
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i32_const(0),
        );
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: false,
                shared: false,
            },
            &ConstExpr::i32_const(PATH_BUF),
        );
        globals.global(
            GlobalType {
                val_type: ValType::I32,
                mutable: false,
                shared: false,
            },
            &ConstExpr::i32_const(PAYLOAD_BUF),
        );
        globals.global(
            GlobalType {
                val_type: str_val(),
                mutable: true,
                shared: false,
            },
            &ConstExpr::ref_null(HeapType::Concrete(T_STR)),
        );
        globals.global(
            GlobalType {
                val_type: ValType::I64,
                mutable: true,
                shared: false,
            },
            &ConstExpr::i64_const(0),
        );
        module.section(&globals);

        // Exports.
        let mut exports = ExportSection::new();
        exports.export("mem", ExportKind::Memory, 0);
        exports.export("init", ExportKind::Func, self.f_init());
        exports.export("dispatch", ExportKind::Func, self.f_dispatch());
        exports.export("path_buf", ExportKind::Global, G_PATH_BUF);
        exports.export("payload_buf", ExportKind::Global, G_PAYLOAD_BUF);
        module.section(&exports);

        module.section(&StartSection {
            function_index: self.f_boot(),
        });

        // array.new_data requires a data-count section before code.
        module.section(&DataCountSection { count: 2 });

        // Code — order must match the function section.
        let mut bodies: Vec<Function> = vec![
            self.fn_w8(),
            self.fn_wvu(),
            self.fn_wstr(),
            self.fn_itoa(),
            self.fn_mem2str(),
            self.fn_concat_str(),
            self.fn_streq(),
            self.fn_wstrgc(),
        ];
        for i in 0..self.records.len() {
            bodies.push(self.fn_arrcat(i));
            bodies.push(self.fn_arrrev(i));
        }
        for j in 0..self.regions.len() {
            bodies.push(self.fn_region(j)?);
        }
        bodies.push(self.fn_update()?);
        bodies.push(self.fn_patches()?);
        bodies.push(self.fn_boot()?);
        bodies.push(self.fn_init()?);
        bodies.push(self.fn_dispatch()?);
        let mut code = CodeSection::new();
        for b in &bodies {
            code.function(b);
        }
        module.section(&code);

        // Data: segment 0 active (streamed by wstr), segment 1 passive
        // (materialized by array.new_data). Same bytes.
        let mut data = DataSection::new();
        data.active(
            0,
            &ConstExpr::i32_const(DATA_BASE),
            self.pool.bytes.iter().copied(),
        );
        data.passive(self.pool.bytes.iter().copied());
        module.section(&data);

        Ok(module.finish())
    }

    // --- emitted stdlib ---------------------------------------------------------

    fn fn_w8(&self) -> Function {
        let mut f = Function::new([]);
        for i in [
            I::GlobalGet(G_CURSOR),
            I::LocalGet(0),
            I::I32Store8(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }),
            I::GlobalGet(G_CURSOR),
            I::I32Const(1),
            I::I32Add,
            I::GlobalSet(G_CURSOR),
            I::End,
        ] {
            f.instruction(&i);
        }
        f
    }

    fn fn_wvu(&self) -> Function {
        let mut f = Function::new([(1, ValType::I32)]);
        for i in [
            I::Loop(BlockType::Empty),
            I::LocalGet(0),
            I::I32Const(0x7f),
            I::I32And,
            I::LocalSet(1),
            I::LocalGet(0),
            I::I32Const(7),
            I::I32ShrU,
            I::LocalSet(0),
            I::LocalGet(0),
            I::If(BlockType::Empty),
            I::LocalGet(1),
            I::I32Const(0x80),
            I::I32Or,
            I::Call(F_W8),
            I::Br(1),
            I::Else,
            I::LocalGet(1),
            I::Call(F_W8),
            I::End,
            I::End,
            I::End,
        ] {
            f.instruction(&i);
        }
        f
    }

    fn fn_wstr(&self) -> Function {
        let mut f = Function::new([]);
        for i in [
            I::LocalGet(1),
            I::Call(F_WVU),
            I::GlobalGet(G_CURSOR),
            I::LocalGet(0),
            I::LocalGet(1),
            I::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            },
            I::GlobalGet(G_CURSOR),
            I::LocalGet(1),
            I::I32Add,
            I::GlobalSet(G_CURSOR),
            I::End,
        ] {
            f.instruction(&i);
        }
        f
    }

    /// itoa(n) -> str. (i64::MIN renders wrong through the negate; fine.)
    fn fn_itoa(&self) -> Function {
        let mut f = Function::new([(2, ValType::I32), (1, str_val()), (1, ValType::I32)]);
        let mut ins: Vec<I<'static>> = Vec::new();
        ins.extend([
            I::LocalGet(0),
            I::I64Const(0),
            I::I64LtS,
            I::If(BlockType::Empty),
            I::I32Const(1),
            I::LocalSet(2),
            I::I64Const(0),
            I::LocalGet(0),
            I::I64Sub,
            I::LocalSet(0),
            I::End,
            I::Loop(BlockType::Empty),
            I::I32Const(SCRATCH),
            I::LocalGet(1),
            I::I32Add,
            I::LocalGet(0),
            I::I64Const(10),
            I::I64RemS,
            I::I32WrapI64,
            I::I32Const(48),
            I::I32Add,
            I::I32Store8(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }),
            I::LocalGet(1),
            I::I32Const(1),
            I::I32Add,
            I::LocalSet(1),
            I::LocalGet(0),
            I::I64Const(10),
            I::I64DivS,
            I::LocalSet(0),
            I::LocalGet(0),
            I::I64Const(0),
            I::I64Ne,
            I::BrIf(0),
            I::End,
            I::LocalGet(1),
            I::LocalGet(2),
            I::I32Add,
            I::ArrayNewDefault(T_STR),
            I::LocalSet(3),
            I::LocalGet(2),
            I::If(BlockType::Empty),
            I::LocalGet(3),
            I::I32Const(0),
            I::I32Const(45),
            I::ArraySet(T_STR),
            I::End,
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::LocalGet(4),
            I::LocalGet(1),
            I::I32GeU,
            I::BrIf(1),
            I::LocalGet(3),
            I::LocalGet(2),
            I::LocalGet(4),
            I::I32Add,
            I::I32Const(SCRATCH),
            I::LocalGet(1),
            I::I32Add,
            I::I32Const(1),
            I::I32Sub,
            I::LocalGet(4),
            I::I32Sub,
            I::I32Load8U(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }),
            I::ArraySet(T_STR),
            I::LocalGet(4),
            I::I32Const(1),
            I::I32Add,
            I::LocalSet(4),
            I::Br(0),
            I::End,
            I::End,
            I::LocalGet(3),
            I::End,
        ]);
        for i in &ins {
            f.instruction(i);
        }
        f
    }

    fn fn_mem2str(&self) -> Function {
        let mut f = Function::new([(1, ValType::I32), (1, str_val())]);
        let mut ins: Vec<I<'static>> = Vec::new();
        ins.extend([
            I::LocalGet(1),
            I::ArrayNewDefault(T_STR),
            I::LocalSet(3),
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::LocalGet(2),
            I::LocalGet(1),
            I::I32GeU,
            I::BrIf(1),
            I::LocalGet(3),
            I::LocalGet(2),
            I::LocalGet(0),
            I::LocalGet(2),
            I::I32Add,
            I::I32Load8U(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }),
            I::ArraySet(T_STR),
            I::LocalGet(2),
            I::I32Const(1),
            I::I32Add,
            I::LocalSet(2),
            I::Br(0),
            I::End,
            I::End,
            I::LocalGet(3),
            I::End,
        ]);
        for i in &ins {
            f.instruction(i);
        }
        f
    }

    fn fn_concat_str(&self) -> Function {
        let mut f = Function::new([(2, ValType::I32), (1, str_val())]);
        let mut ins: Vec<I<'static>> = Vec::new();
        ins.extend([
            I::LocalGet(0),
            I::ArrayLen,
            I::LocalSet(2),
            I::LocalGet(1),
            I::ArrayLen,
            I::LocalSet(3),
            I::LocalGet(2),
            I::LocalGet(3),
            I::I32Add,
            I::ArrayNewDefault(T_STR),
            I::LocalSet(4),
            I::LocalGet(4),
            I::I32Const(0),
            I::LocalGet(0),
            I::I32Const(0),
            I::LocalGet(2),
            I::ArrayCopy {
                array_type_index_dst: T_STR,
                array_type_index_src: T_STR,
            },
            I::LocalGet(4),
            I::LocalGet(2),
            I::LocalGet(1),
            I::I32Const(0),
            I::LocalGet(3),
            I::ArrayCopy {
                array_type_index_dst: T_STR,
                array_type_index_src: T_STR,
            },
            I::LocalGet(4),
            I::End,
        ]);
        for i in &ins {
            f.instruction(i);
        }
        f
    }

    fn fn_streq(&self) -> Function {
        let mut f = Function::new([(2, ValType::I32)]);
        let mut ins: Vec<I<'static>> = Vec::new();
        ins.extend([
            I::LocalGet(0),
            I::ArrayLen,
            I::LocalSet(3),
            I::LocalGet(3),
            I::LocalGet(1),
            I::ArrayLen,
            I::I32Ne,
            I::If(BlockType::Empty),
            I::I32Const(0),
            I::Return,
            I::End,
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::LocalGet(2),
            I::LocalGet(3),
            I::I32GeU,
            I::BrIf(1),
            I::LocalGet(0),
            I::LocalGet(2),
            I::ArrayGetU(T_STR),
            I::LocalGet(1),
            I::LocalGet(2),
            I::ArrayGetU(T_STR),
            I::I32Ne,
            I::If(BlockType::Empty),
            I::I32Const(0),
            I::Return,
            I::End,
            I::LocalGet(2),
            I::I32Const(1),
            I::I32Add,
            I::LocalSet(2),
            I::Br(0),
            I::End,
            I::End,
            I::I32Const(1),
            I::End,
        ]);
        for i in &ins {
            f.instruction(i);
        }
        f
    }

    fn fn_wstrgc(&self) -> Function {
        let mut f = Function::new([(2, ValType::I32)]);
        let mut ins: Vec<I<'static>> = Vec::new();
        ins.extend([
            I::LocalGet(0),
            I::ArrayLen,
            I::LocalSet(2),
            I::LocalGet(2),
            I::Call(F_WVU),
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::LocalGet(1),
            I::LocalGet(2),
            I::I32GeU,
            I::BrIf(1),
            I::LocalGet(0),
            I::LocalGet(1),
            I::ArrayGetU(T_STR),
            I::Call(F_W8),
            I::LocalGet(1),
            I::I32Const(1),
            I::I32Add,
            I::LocalSet(1),
            I::Br(0),
            I::End,
            I::End,
            I::End,
        ]);
        for i in &ins {
            f.instruction(i);
        }
        f
    }

    /// Per-record array concat: (a, b) -> new array.
    fn fn_arrcat(&self, rec: usize) -> Function {
        let arr = 1 + self.n_records() + rec as u32;
        let arr_vt = ValType::Ref(nullable(arr));
        let mut f = Function::new([(2, ValType::I32), (1, arr_vt)]);
        let mut ins: Vec<I<'static>> = Vec::new();
        ins.extend([
            I::LocalGet(0),
            I::ArrayLen,
            I::LocalSet(2),
            I::LocalGet(1),
            I::ArrayLen,
            I::LocalSet(3),
            I::LocalGet(2),
            I::LocalGet(3),
            I::I32Add,
            I::ArrayNewDefault(arr),
            I::LocalSet(4),
            I::LocalGet(4),
            I::I32Const(0),
            I::LocalGet(0),
            I::I32Const(0),
            I::LocalGet(2),
            I::ArrayCopy {
                array_type_index_dst: arr,
                array_type_index_src: arr,
            },
            I::LocalGet(4),
            I::LocalGet(2),
            I::LocalGet(1),
            I::I32Const(0),
            I::LocalGet(3),
            I::ArrayCopy {
                array_type_index_dst: arr,
                array_type_index_src: arr,
            },
            I::LocalGet(4),
            I::End,
        ]);
        for i in &ins {
            f.instruction(i);
        }
        f
    }

    /// Per-record array reverse: (a) -> new array.
    fn fn_arrrev(&self, rec: usize) -> Function {
        let arr = 1 + self.n_records() + rec as u32;
        let arr_vt = ValType::Ref(nullable(arr));
        let mut f = Function::new([(2, ValType::I32), (1, arr_vt)]);
        let mut ins: Vec<I<'static>> = Vec::new();
        // locals: 1 = n, 2 = i, 3 = out (params: 0 = a)
        ins.extend([
            I::LocalGet(0),
            I::ArrayLen,
            I::LocalSet(1),
            I::LocalGet(1),
            I::ArrayNewDefault(arr),
            I::LocalSet(3),
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::LocalGet(2),
            I::LocalGet(1),
            I::I32GeU,
            I::BrIf(1),
            I::LocalGet(3),
            I::LocalGet(2),
            I::LocalGet(0),
            I::LocalGet(1),
            I::I32Const(1),
            I::I32Sub,
            I::LocalGet(2),
            I::I32Sub,
            I::ArrayGet(arr),
            I::ArraySet(arr),
            I::LocalGet(2),
            I::I32Const(1),
            I::I32Add,
            I::LocalSet(2),
            I::Br(0),
            I::End,
            I::End,
            I::LocalGet(3),
            I::End,
        ]);
        for i in &ins {
            f.instruction(i);
        }
        f
    }

    // --- app functions -----------------------------------------------------------

    /// region_j(): serialize the region's parent element wholesale — tag,
    /// attrs, then one body element per list item with the loop var bound.
    fn fn_region(&mut self, j: usize) -> Result<Function, String> {
        let region = &self.regions[j];
        let (tag, attrs, var, list, rec, body) = (
            region.parent_tag.clone(),
            region.parent_attrs.clone(),
            region.var.clone(),
            region.list.clone(),
            region.record.clone(),
            region.body.clone(),
        );
        let arr_idx = self.arr_idx(&rec);
        let arr_vt = ValType::Ref(nullable(arr_idx));
        let rec_vt = ValType::Ref(nullable(self.rec_idx(&rec)));

        let mut ctx = Ctx::new(4); // fixed locals below occupy 0..=3
        let mut ins: Vec<I<'static>> = Vec::new();
        // fixed locals: 0 = src, 1 = n, 2 = i, 3 = t
        Self::c_w8(&mut ins, 1);
        self.c_wstr(&mut ins, &tag);
        Self::c_wvu(&mut ins, attrs.len() as u32);
        for (name, value) in &attrs {
            let name = name.clone();
            self.c_wstr(&mut ins, &name);
            self.c_text(value, &mut ctx, &mut ins)?;
        }
        self.list_expr(&list, &mut ctx, &mut ins)?;
        ins.push(I::LocalSet(0));
        ins.push(I::LocalGet(0));
        ins.push(I::ArrayLen);
        ins.push(I::LocalSet(1));
        ins.push(I::LocalGet(1));
        ins.push(I::Call(F_WVU)); // children count
        ins.push(I::Block(BlockType::Empty));
        ins.push(I::Loop(BlockType::Empty));
        ins.push(I::LocalGet(2));
        ins.push(I::LocalGet(1));
        ins.push(I::I32GeU);
        ins.push(I::BrIf(1));
        ins.push(I::LocalGet(0));
        ins.push(I::LocalGet(2));
        ins.push(I::ArrayGet(arr_idx));
        ins.push(I::LocalSet(3));
        ctx.env.push((var.clone(), Slot::Item(3, rec.clone())));
        self.emit_body_element(&body, &mut ctx, &mut ins)?;
        ctx.env.pop();
        ins.push(I::LocalGet(2));
        ins.push(I::I32Const(1));
        ins.push(I::I32Add);
        ins.push(I::LocalSet(2));
        ins.push(I::Br(0));
        ins.push(I::End);
        ins.push(I::End);
        ins.push(I::End);
        Ok(ctx.into_function(&[arr_vt, ValType::I32, ValType::I32, rec_vt], &ins))
    }

    /// Serialize one region-body element (dynamics evaluated inline; no
    /// separate patches — the region replaces wholesale).
    fn emit_body_element(
        &mut self,
        el: &Element,
        ctx: &mut Ctx,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        Self::c_w8(out, 1);
        let tag = el.tag.clone();
        self.c_wstr(out, &tag);
        let attrs: Vec<(String, Expr)> = el
            .attrs
            .iter()
            .filter_map(|a| match a {
                Attr::Str { name, value, .. } if name != "key" => {
                    Some((name.clone(), value.clone()))
                }
                _ => None,
            })
            .collect();
        Self::c_wvu(out, attrs.len() as u32);
        for (name, value) in &attrs {
            let name = name.clone();
            self.c_wstr(out, &name);
            self.c_text(value, ctx, out)?;
        }
        Self::c_wvu(out, el.children.len() as u32);
        for child in &el.children {
            match child {
                Child::Elem(e) => self.emit_body_element(e, ctx, out)?,
                Child::Text(expr, _) => {
                    Self::c_w8(out, 0);
                    let expr = expr.clone();
                    self.c_text(&expr, ctx, out)?;
                }
                Child::For { pos, .. } => return Err(unsupported(*pos, "nested `for` regions")),
            }
        }
        Ok(())
    }

    /// update(msg): record-update semantics against the pre-update model.
    fn fn_update(&mut self) -> Result<Function, String> {
        let updates = self.app.updates.clone();
        let nfields = self.fields.len();
        let field_types: Vec<ValType> = self
            .fields
            .clone()
            .iter()
            .map(|(_, t)| self.val_type(t))
            .collect();
        let mut ctx = Ctx::new(1 + nfields as u32);
        let mut ins: Vec<I<'static>> = Vec::new();
        for u in &updates {
            let k = self.msg_index(&u.msg);
            if let Some((v, _)) = &u.var {
                let slot = match self.msg_payload(k) {
                    Some(Ty::Str) => Slot::PayloadStr,
                    Some(Ty::Int) => Slot::PayloadInt,
                    _ => unreachable!("gated payload types"),
                };
                ctx.env.push((v.clone(), slot));
            }
            ins.push(I::LocalGet(0));
            ins.push(I::I32Const(k as i32));
            ins.push(I::I32Eq);
            ins.push(I::If(BlockType::Empty));
            for (field, expr, _) in &u.fields {
                let ty = self.field_ty(field);
                self.val_expr(expr, &ty, &mut ctx, &mut ins)?;
                ins.push(I::LocalSet(1 + self.field_index(field)));
            }
            for (field, _, _) in &u.fields {
                let idx = self.field_index(field);
                ins.push(I::GlobalGet(G_STATE));
                ins.push(I::LocalGet(1 + idx));
                ins.push(I::StructSet {
                    struct_type_index: self.model_idx(),
                    field_index: idx,
                });
            }
            ins.push(I::Return);
            ins.push(I::End);
            if u.var.is_some() {
                ctx.env.pop();
            }
        }
        ins.push(I::End);
        Ok(ctx.into_function(&field_types, &ins))
    }

    /// patches(): SetText + SetAttr for scalar dynamics, then one Replace
    /// per region, all unconditional. (Value caching is a later trim.)
    fn fn_patches(&mut self) -> Result<Function, String> {
        let texts: Vec<(Vec<u32>, Expr)> = self
            .dyn_texts
            .iter()
            .map(|d| (d.path.clone(), d.expr.clone()))
            .collect();
        let attrs: Vec<(Vec<u32>, String, Expr)> = self
            .dyn_attrs
            .iter()
            .map(|d| (d.path.clone(), d.name.clone(), d.expr.clone()))
            .collect();
        let regions: Vec<Vec<u32>> = self.regions.iter().map(|r| r.path.clone()).collect();
        let mut ctx = Ctx::new(0);
        let mut ins: Vec<I<'static>> = Vec::new();
        Self::c_w8(&mut ins, WIRE_VERSION);
        Self::c_wvu(&mut ins, (texts.len() + attrs.len() + regions.len()) as u32);
        for (path, expr) in &texts {
            Self::c_w8(&mut ins, 1); // setText
            Self::c_wvu(&mut ins, path.len() as u32);
            for seg in path {
                Self::c_wvu(&mut ins, *seg);
            }
            self.c_text(expr, &mut ctx, &mut ins)?;
        }
        for (path, name, expr) in &attrs {
            Self::c_w8(&mut ins, 2); // setAttr
            Self::c_wvu(&mut ins, path.len() as u32);
            for seg in path {
                Self::c_wvu(&mut ins, *seg);
            }
            self.c_wstr(&mut ins, name);
            self.c_text(expr, &mut ctx, &mut ins)?;
        }
        for (j, path) in regions.iter().enumerate() {
            Self::c_w8(&mut ins, 0); // replace
            Self::c_wvu(&mut ins, path.len() as u32);
            for seg in path {
                Self::c_wvu(&mut ins, *seg);
            }
            ins.push(I::Call(self.f_region(j)));
        }
        for _ in 0..3 {
            Self::c_wvu(&mut ins, 0); // events, cmds, subs
        }
        ins.push(I::End);
        Ok(ctx.into_function(&[], &ins))
    }

    fn fn_boot(&mut self) -> Result<Function, String> {
        let inits: Vec<(Expr, Ty)> = self
            .fields
            .clone()
            .iter()
            .map(|(name, ty)| {
                let expr = self
                    .app
                    .init
                    .iter()
                    .find(|(n, _, _)| n == name)
                    .map(|(_, e, _)| e.clone())
                    .expect("typechecked: init is total");
                (expr, ty.clone())
            })
            .collect();
        let mut ctx = Ctx::new(0);
        let mut ins: Vec<I<'static>> = Vec::new();
        for (expr, ty) in &inits {
            self.val_expr(expr, ty, &mut ctx, &mut ins)?;
        }
        ins.push(I::StructNew(self.model_idx()));
        ins.push(I::GlobalSet(G_STATE));
        ins.push(I::End);
        Ok(ctx.into_function(&[], &ins))
    }

    fn fn_init(&mut self) -> Result<Function, String> {
        let mut ins: Vec<I<'static>> = Vec::new();
        ins.push(I::I32Const(0));
        ins.push(I::GlobalSet(G_CURSOR));
        Self::c_w8(&mut ins, WIRE_VERSION);
        ins.extend(self.tree.clone());
        let events: Vec<(String, bool)> =
            self.events.iter().map(|(n, pd)| (n.clone(), *pd)).collect();
        Self::c_wvu(&mut ins, events.len() as u32);
        for (name, pd) in events {
            self.c_wstr(&mut ins, &name);
            Self::c_w8(&mut ins, pd as u8);
        }
        Self::c_wvu(&mut ins, 0);
        Self::c_wvu(&mut ins, 0);
        ins.push(I::GlobalGet(G_CURSOR));
        ins.push(I::End);
        // The walk already allocated any temps it needed.
        let locals: Vec<(u32, ValType)> = self.init_ctx_locals.iter().map(|t| (1u32, *t)).collect();
        let mut f = Function::new(locals);
        for i in &ins {
            f.instruction(i);
        }
        Ok(f)
    }

    /// dispatch(event_idx, path_len, payload_len): deepest-first prefix
    /// match; region handlers bind the clicked item from the path index and
    /// evaluate the message argument at dispatch time.
    fn fn_dispatch(&mut self) -> Result<Function, String> {
        let handlers: Vec<(Vec<u32>, String, u32, HandlerKindOwned)> = self
            .handlers
            .iter()
            .map(|h| {
                (
                    h.prefix.clone(),
                    h.event.clone(),
                    h.msg,
                    match &h.kind {
                        HandlerKind::Static { takes_input, arg } => HandlerKindOwned::Static {
                            takes_input: *takes_input,
                            arg: arg.clone(),
                        },
                        HandlerKind::ForItem {
                            region,
                            suffix,
                            arg,
                            takes_input,
                        } => HandlerKindOwned::ForItem {
                            region: *region,
                            suffix: suffix.clone(),
                            arg: arg.clone(),
                            takes_input: *takes_input,
                        },
                    },
                )
            })
            .collect();
        let mut ctx = Ctx::new(3);
        let mut ins: Vec<I<'static>> = Vec::new();
        ins.push(I::I32Const(0));
        ins.push(I::GlobalSet(G_CURSOR));
        for (prefix, event, msg, kind) in &handlers {
            ins.push(I::Block(BlockType::Empty));
            ins.push(I::LocalGet(0));
            ins.push(I::I32Const(self.event_code(event)));
            ins.push(I::I32Ne);
            ins.push(I::BrIf(0));
            let total = match kind {
                HandlerKindOwned::Static { .. } => prefix.len(),
                HandlerKindOwned::ForItem { suffix, .. } => prefix.len() + 1 + suffix.len(),
            };
            ins.push(I::LocalGet(1));
            ins.push(I::I32Const(total as i32));
            ins.push(I::I32LtU);
            ins.push(I::BrIf(0));
            for (i, seg) in prefix.iter().enumerate() {
                ins.push(I::I32Const(PATH_BUF + 4 * i as i32));
                ins.push(I::I32Load(MemArg {
                    offset: 0,
                    align: 2,
                    memory_index: 0,
                }));
                ins.push(I::I32Const(*seg as i32));
                ins.push(I::I32Ne);
                ins.push(I::BrIf(0));
            }
            match kind {
                HandlerKindOwned::Static { takes_input, arg } => {
                    if *takes_input {
                        ins.push(I::I32Const(PAYLOAD_BUF));
                        ins.push(I::LocalGet(2));
                        ins.push(I::Call(F_MEM2STR));
                        ins.push(I::GlobalSet(G_PAYLOAD));
                    }
                    if let Some(arg) = arg {
                        self.emit_arg(arg, *msg, &mut ctx, &mut ins)?;
                    }
                }
                HandlerKindOwned::ForItem {
                    region,
                    suffix,
                    arg,
                    takes_input,
                } => {
                    let r = &self.regions[*region];
                    let (var, list, rec) = (r.var.clone(), r.list.clone(), r.record.clone());
                    let arr_idx = self.arr_idx(&rec);
                    let src = ctx.tmp(ValType::Ref(nullable(arr_idx)));
                    let kk = ctx.tmp(ValType::I32);
                    let t = ctx.tmp(ValType::Ref(nullable(self.rec_idx(&rec))));
                    for (i, seg) in suffix.iter().enumerate() {
                        ins.push(I::I32Const(PATH_BUF + 4 * (prefix.len() + 1 + i) as i32));
                        ins.push(I::I32Load(MemArg {
                            offset: 0,
                            align: 2,
                            memory_index: 0,
                        }));
                        ins.push(I::I32Const(*seg as i32));
                        ins.push(I::I32Ne);
                        ins.push(I::BrIf(0));
                    }
                    // k = path[prefix.len()]; bounds-check against the list.
                    self.list_expr(&list, &mut ctx, &mut ins)?;
                    ins.push(I::LocalSet(src));
                    ins.push(I::I32Const(PATH_BUF + 4 * prefix.len() as i32));
                    ins.push(I::I32Load(MemArg {
                        offset: 0,
                        align: 2,
                        memory_index: 0,
                    }));
                    ins.push(I::LocalSet(kk));
                    ins.push(I::LocalGet(kk));
                    ins.push(I::LocalGet(src));
                    ins.push(I::ArrayLen);
                    ins.push(I::I32GeU);
                    ins.push(I::BrIf(0));
                    ins.push(I::LocalGet(src));
                    ins.push(I::LocalGet(kk));
                    ins.push(I::ArrayGet(arr_idx));
                    ins.push(I::LocalSet(t));
                    if *takes_input {
                        ins.push(I::I32Const(PAYLOAD_BUF));
                        ins.push(I::LocalGet(2));
                        ins.push(I::Call(F_MEM2STR));
                        ins.push(I::GlobalSet(G_PAYLOAD));
                    }
                    if let Some(arg) = arg {
                        ctx.env.push((var, Slot::Item(t, rec)));
                        let res = self.emit_arg(arg, *msg, &mut ctx, &mut ins);
                        ctx.env.pop();
                        res?;
                    }
                }
            }
            ins.push(I::I32Const(*msg as i32));
            ins.push(I::Call(self.f_update()));
            ins.push(I::Call(self.f_patches()));
            ins.push(I::GlobalGet(G_CURSOR));
            ins.push(I::Return);
            ins.push(I::End);
        }
        Self::c_w8(&mut ins, WIRE_VERSION);
        for _ in 0..4 {
            Self::c_wvu(&mut ins, 0);
        }
        ins.push(I::GlobalGet(G_CURSOR));
        ins.push(I::End);
        Ok(ctx.into_function(&[], &ins))
    }

    /// Evaluate a handler's message argument into the right payload global.
    fn emit_arg(
        &mut self,
        arg: &Expr,
        msg: u32,
        ctx: &mut Ctx,
        ins: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        match self.msg_payload(msg) {
            Some(Ty::Int) => {
                self.int_expr(arg, ctx, ins)?;
                ins.push(I::GlobalSet(G_PAYLOAD_I64));
            }
            Some(Ty::Str) => {
                self.str_expr(arg, ctx, ins)?;
                ins.push(I::GlobalSet(G_PAYLOAD));
            }
            _ => return Err(unsupported(pos_of(arg), "this message argument")),
        }
        Ok(())
    }
}

/// Owned mirror of HandlerKind so dispatch can iterate without aliasing self.
enum HandlerKindOwned {
    Static {
        takes_input: bool,
        arg: Option<Expr>,
    },
    ForItem {
        region: usize,
        suffix: Vec<u32>,
        arg: Option<Expr>,
        takes_input: bool,
    },
}

fn pos_of(e: &Expr) -> Pos {
    match e {
        Expr::Var(_, p)
        | Expr::Field(_, _, p)
        | Expr::Show(_, p)
        | Expr::Len(_, p)
        | Expr::Sum(_, p)
        | Expr::ToInt(_, p)
        | Expr::Nth(_, _, _, p)
        | Expr::Head(_, p)
        | Expr::None(p)
        | Expr::Some(_, p)
        | Expr::Case { pos: p, .. }
        | Expr::Reverse(_, p)
        | Expr::Not(_, p)
        | Expr::Bin(_, _, _, p)
        | Expr::If(_, _, _, p)
        | Expr::ListLit(_, p)
        | Expr::RecordLit(_, p)
        | Expr::RecordUpdate(_, _, p)
        | Expr::For { pos: p, .. } => *p,
        _ => Pos { line: 0, col: 0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit_src(src: &str) -> Result<Vec<u8>, String> {
        emit(&zumar_lang::compile(src).unwrap())
    }

    fn assert_valid(src: &str) -> Vec<u8> {
        let bytes = emit_src(src).unwrap();
        wasmparser::Validator::new()
            .validate_all(&bytes)
            .unwrap_or_else(|e| panic!("emitted module is invalid: {e}"));
        bytes
    }

    #[test]
    fn counter_emits_a_valid_gc_module() {
        assert_valid(include_str!("../../../examples/lang-counter/counter.zu"));
    }

    #[test]
    fn hello_with_gc_strings_emits_a_valid_module() {
        assert_valid(include_str!("../../../examples/lang-hello/hello.zu"));
    }

    #[test]
    fn todo_with_records_and_regions_emits_a_valid_module() {
        let bytes = assert_valid(include_str!("../../../examples/lang-todo/todo.zu"));
        assert!(
            bytes.len() < 16384,
            "unexpectedly large: {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn maybe_still_errors_cleanly() {
        let src = r#"
app M
record C { id: Int }
model { pick: Maybe C }
init = { pick = none }
msg Set
update Set = { pick = none }
view = div [] []
"#;
        let app = zumar_lang::compile(src).unwrap();
        let err = emit(&app).unwrap_err();
        assert!(err.contains("not yet in the wasmgc backend"), "{err}");
    }

    #[test]
    fn effects_error_cleanly() {
        let src = r#"
app E
model { n: Int }
init = { n = 0 }
msg M | P
update M = { n = 1 } then delay(100, P)
update P = { n = 2 }
view = div [] []
"#;
        let app = zumar_lang::compile(src).unwrap();
        let err = emit(&app).unwrap_err();
        assert!(err.contains("effects"), "{err}");
    }

    #[test]
    fn bool_payloads_error_cleanly() {
        let src = r#"
app B
model { on: Int }
init = { on = 0 }
msg Flip Bool
update Flip b = { on = if b then 1 else 0 }
view = div [] []
"#;
        let app = zumar_lang::compile(src).unwrap();
        let err = emit(&app).unwrap_err();
        assert!(err.contains("not yet in the wasmgc backend"), "{err}");
    }
}
