#![forbid(unsafe_code)]
//! WasmGC backend spike: emit a self-contained GC module from the zumar-lang
//! AST via `wasm-encoder`. No Rust toolchain, no wasm-bindgen, no runtime
//! crate — the module is the whole app.
//!
//! The load-bearing idea: `.zu` views are statically known, so the compiler
//! emits a **compile-time patch plan** instead of shipping a diff. Every
//! dynamic text node's path is known at compile time; `dispatch` runs the
//! update on a GC struct and re-serializes exactly those texts as SetText
//! patches. No vdom in memory, no runtime.wasm — which retires the open
//! question from the first spike.
//!
//! Boundary (raw exports, no glue):
//! - `init() -> len` — wire-encoded InitialRender at mem[0..len]
//! - `dispatch(event_idx, path_len) -> len` — event_idx indexes the events
//!   array of the init message; the host writes the path's u32s at
//!   `path_buf` first.
//! - `mem`, `path_buf` exports.
//!
//! Handler resolution mirrors the runtime's bubbling: handlers are matched
//! deepest-first by path prefix. Update messages mutate the model struct
//! (GC heap); the wire buffer lives in linear memory.
//!
//! Subset (the counter shape): Int model fields, payload-less messages,
//! static element tree, literal string attributes, text children that are
//! string literals or Int-derived (`show`, `if`). Everything else reports a
//! clear "not yet in the wasmgc backend" error — the Rust backend remains
//! the default.

use std::collections::BTreeMap;

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataSection, ExportKind, ExportSection, FieldType, Function,
    FunctionSection, GlobalSection, GlobalType, HeapType, Instruction as I, MemArg, MemorySection,
    MemoryType, Module, RefType, StartSection, StorageType, TypeSection, ValType,
};
use zumar_lang::ast::{App, Attr, Child, Element, Expr, Op, Pos};

const WIRE_VERSION: u8 = 1;

// Memory layout: wire buffer grows from 0; itoa scratch; string constants
// (active data segment); path staging written by the host.
const SCRATCH: i32 = 3072;
const DATA_BASE: i32 = 4096;
const PATH_BUF: i32 = 60000;

// Type indices.
const T_MODEL: u32 = 0;
const T_I32: u32 = 1; // (i32) -> ()
const T_II: u32 = 2; // (i32, i32) -> ()
const T_I64: u32 = 3; // (i64) -> ()
const T_VOID: u32 = 4; // () -> ()
const T_RET: u32 = 5; // () -> i32
const T_DISPATCH: u32 = 6; // (i32, i32) -> i32

// Function indices (order of bodies below).
const F_W8: u32 = 0;
const F_WVU: u32 = 1;
const F_WSTR: u32 = 2;
const F_WITOA: u32 = 3;
const F_UPDATE: u32 = 4;
const F_PATCHES: u32 = 5;
const F_BOOT: u32 = 6;
const F_INIT: u32 = 7;
const F_DISPATCH: u32 = 8;

// Globals.
const G_STATE: u32 = 0;
const G_CURSOR: u32 = 1;
const G_PATH_BUF: u32 = 2;

pub fn emit(app: &App) -> Result<Vec<u8>, String> {
    Emitter::build(app)?.module()
}

fn unsupported(pos: Pos, what: &str) -> String {
    format!(
        "{}:{}: {what} is not yet in the wasmgc backend (use the default Rust backend)",
        pos.line, pos.col
    )
}

struct Pool {
    bytes: Vec<u8>,
    map: BTreeMap<String, (i32, i32)>,
}

impl Pool {
    fn intern(&mut self, s: &str) -> (i32, i32) {
        if let Some(&hit) = self.map.get(s) {
            return hit;
        }
        let off = DATA_BASE + self.bytes.len() as i32;
        self.bytes.extend_from_slice(s.as_bytes());
        let entry = (off, s.len() as i32);
        self.map.insert(s.to_string(), entry);
        entry
    }
}

struct DynText {
    path: Vec<u32>,
    expr: Expr,
}

struct Handler {
    path: Vec<u32>,
    event: String,
    msg: u32,
}

struct Emitter<'a> {
    app: &'a App,
    fields: Vec<String>,
    msgs: Vec<String>,
    pool: Pool,
    tree: Vec<I<'static>>,
    dyns: Vec<DynText>,
    handlers: Vec<Handler>,
    events: BTreeMap<String, bool>,
}

impl<'a> Emitter<'a> {
    fn build(app: &'a App) -> Result<Emitter<'a>, String> {
        if let Some(r) = app.records.first() {
            return Err(unsupported(r.pos, "record types"));
        }
        for (name, ty, pos) in &app.model {
            if *ty != zumar_lang::ast::Ty::Int {
                return Err(unsupported(*pos, &format!("model field `{name}: {ty}`")));
            }
        }
        for m in &app.msgs {
            if m.payload.is_some() {
                return Err(unsupported(m.pos, "message payloads"));
            }
        }
        let mut e = Emitter {
            app,
            fields: app.model.iter().map(|(n, _, _)| n.clone()).collect(),
            msgs: app.msgs.iter().map(|m| m.name.clone()).collect(),
            pool: Pool {
                bytes: Vec::new(),
                map: BTreeMap::new(),
            },
            tree: Vec::new(),
            dyns: Vec::new(),
            handlers: Vec::new(),
            events: BTreeMap::new(),
        };
        let mut tree = Vec::new();
        let mut path = Vec::new();
        e.walk(&app.view, &mut path, &mut tree)?;
        e.tree = tree;
        // Deepest-first so bubbling picks the innermost handler.
        e.handlers.sort_by_key(|h| std::cmp::Reverse(h.path.len()));
        Ok(e)
    }

    fn field_index(&self, name: &str) -> u32 {
        self.fields
            .iter()
            .position(|f| f == name)
            .expect("typechecked") as u32
    }

    fn msg_index(&self, name: &str) -> u32 {
        self.msgs
            .iter()
            .position(|m| m == name)
            .expect("typechecked") as u32
    }

    fn event_code(&self, name: &str) -> i32 {
        self.events
            .keys()
            .position(|e| e == name)
            .expect("collected") as i32
    }

    // --- write helpers (instruction sequences) ----------------------------

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
        out.push(I::I32Const(off));
        out.push(I::I32Const(len));
        out.push(I::Call(F_WSTR));
    }

    // --- view walk ---------------------------------------------------------

    fn walk(
        &mut self,
        el: &Element,
        path: &mut Vec<u32>,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        Self::c_w8(out, 1); // element
        let tag = el.tag.clone();
        self.c_wstr(out, &tag);

        let mut str_attrs = Vec::new();
        for attr in &el.attrs {
            match attr {
                Attr::Str { name, value, pos } => {
                    if name == "key" {
                        return Err(unsupported(*pos, "keyed elements"));
                    }
                    let Expr::Str(v) = value else {
                        return Err(unsupported(*pos, "computed attribute values"));
                    };
                    str_attrs.push((name.clone(), v.clone()));
                }
                Attr::On {
                    event,
                    handler,
                    prevent_default,
                } => {
                    if handler.arg.is_some() {
                        return Err(unsupported(handler.pos, "message arguments"));
                    }
                    let msg = self.msg_index(&handler.name);
                    let pd = self.events.entry(event.clone()).or_insert(false);
                    *pd = *pd || *prevent_default;
                    self.handlers.push(Handler {
                        path: path.clone(),
                        event: event.clone(),
                        msg,
                    });
                }
                Attr::OnValue { pos, .. } => return Err(unsupported(*pos, "input events")),
            }
        }
        Self::c_wvu(out, str_attrs.len() as u32);
        for (name, value) in str_attrs {
            self.c_wstr(out, &name);
            self.c_wstr(out, &value);
        }

        Self::c_wvu(out, el.children.len() as u32);
        for (i, child) in el.children.iter().enumerate() {
            path.push(i as u32);
            match child {
                Child::Elem(e) => self.walk(e, path, out)?,
                Child::Text(expr, _) => {
                    Self::c_w8(out, 0); // text node
                    if !matches!(expr, Expr::Str(_)) {
                        self.dyns.push(DynText {
                            path: path.clone(),
                            expr: expr.clone(),
                        });
                    }
                    let expr = expr.clone();
                    self.str_expr(&expr, out)?;
                }
                Child::For { pos, .. } => return Err(unsupported(*pos, "list rendering")),
            }
            path.pop();
        }
        Ok(())
    }

    // --- expressions ---------------------------------------------------------

    /// Push an i64 onto the wasm stack.
    fn int_expr(&mut self, e: &Expr, out: &mut Vec<I<'static>>) -> Result<(), String> {
        match e {
            Expr::Int(n) => out.push(I::I64Const(*n)),
            Expr::Field(base, field, pos) => {
                let Expr::Var(m, _) = base.as_ref() else {
                    return Err(unsupported(*pos, "nested field access"));
                };
                if m != "model" {
                    return Err(unsupported(*pos, "variables other than `model`"));
                }
                out.push(I::GlobalGet(G_STATE));
                out.push(I::StructGet {
                    struct_type_index: T_MODEL,
                    field_index: self.field_index(field),
                });
            }
            Expr::Bin(op @ (Op::Add | Op::Sub | Op::Mul), l, r, _) => {
                self.int_expr(l, out)?;
                self.int_expr(r, out)?;
                out.push(match op {
                    Op::Add => I::I64Add,
                    Op::Sub => I::I64Sub,
                    _ => I::I64Mul,
                });
            }
            Expr::If(c, t, f, _) => {
                self.bool_expr(c, out)?;
                out.push(I::If(BlockType::Result(ValType::I64)));
                self.int_expr(t, out)?;
                out.push(I::Else);
                self.int_expr(f, out)?;
                out.push(I::End);
            }
            other => return Err(unsupported(pos_of(other), "this expression")),
        }
        Ok(())
    }

    /// Push an i32 boolean onto the wasm stack.
    fn bool_expr(&mut self, e: &Expr, out: &mut Vec<I<'static>>) -> Result<(), String> {
        match e {
            Expr::Bool(b) => out.push(I::I32Const(*b as i32)),
            Expr::Not(inner, _) => {
                self.bool_expr(inner, out)?;
                out.push(I::I32Eqz);
            }
            Expr::Bin(op @ (Op::Eq | Op::Ne | Op::Lt | Op::Gt), l, r, _) => {
                self.int_expr(l, out)?;
                self.int_expr(r, out)?;
                out.push(match op {
                    Op::Eq => I::I64Eq,
                    Op::Ne => I::I64Ne,
                    Op::Lt => I::I64LtS,
                    _ => I::I64GtS,
                });
            }
            other => return Err(unsupported(pos_of(other), "this condition")),
        }
        Ok(())
    }

    /// Write a varint-length-prefixed string into the wire buffer.
    fn str_expr(&mut self, e: &Expr, out: &mut Vec<I<'static>>) -> Result<(), String> {
        match e {
            Expr::Str(s) => {
                let s = s.clone();
                self.c_wstr(out, &s);
            }
            Expr::Show(inner, _) => {
                self.int_expr(inner, out)?;
                out.push(I::Call(F_WITOA));
            }
            Expr::If(c, t, f, _) => {
                self.bool_expr(c, out)?;
                out.push(I::If(BlockType::Empty));
                self.str_expr(t, out)?;
                out.push(I::Else);
                self.str_expr(f, out)?;
                out.push(I::End);
            }
            other => return Err(unsupported(pos_of(other), "this text expression")),
        }
        Ok(())
    }

    // --- module assembly -----------------------------------------------------

    fn module(mut self) -> Result<Vec<u8>, String> {
        let mut module = Module::new();

        // Types.
        let mut types = TypeSection::new();
        types.ty().struct_(
            self.fields
                .iter()
                .map(|_| FieldType {
                    element_type: StorageType::Val(ValType::I64),
                    mutable: true,
                })
                .collect::<Vec<_>>(),
        );
        types.ty().function([ValType::I32], []);
        types.ty().function([ValType::I32, ValType::I32], []);
        types.ty().function([ValType::I64], []);
        types.ty().function([], []);
        types.ty().function([], [ValType::I32]);
        types
            .ty()
            .function([ValType::I32, ValType::I32], [ValType::I32]);
        module.section(&types);

        // Functions.
        let mut funcs = FunctionSection::new();
        for ty in [
            T_I32, T_I32, T_II, T_I64, T_I32, T_VOID, T_VOID, T_RET, T_DISPATCH,
        ] {
            funcs.function(ty);
        }
        module.section(&funcs);

        // Memory: 2 pages (wire buffer + constants + path staging).
        let mut memory = MemorySection::new();
        memory.memory(MemoryType {
            minimum: 2,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        module.section(&memory);

        // Globals: model ref, wire cursor, path staging offset (exported).
        let model_ref = RefType {
            nullable: true,
            heap_type: HeapType::Concrete(T_MODEL),
        };
        let mut globals = GlobalSection::new();
        globals.global(
            GlobalType {
                val_type: ValType::Ref(model_ref),
                mutable: true,
                shared: false,
            },
            &ConstExpr::ref_null(HeapType::Concrete(T_MODEL)),
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
        module.section(&globals);

        // Exports.
        let mut exports = ExportSection::new();
        exports.export("mem", ExportKind::Memory, 0);
        exports.export("init", ExportKind::Func, F_INIT);
        exports.export("dispatch", ExportKind::Func, F_DISPATCH);
        exports.export("path_buf", ExportKind::Global, G_PATH_BUF);
        module.section(&exports);

        module.section(&StartSection {
            function_index: F_BOOT,
        });

        // Code — order must match the function section.
        let update = self.fn_update()?;
        let patches = self.fn_patches()?;
        let boot = self.fn_boot()?;
        let init = self.fn_init()?;
        let dispatch = self.fn_dispatch();
        let mut code = CodeSection::new();
        code.function(&self.fn_w8());
        code.function(&self.fn_wvu());
        code.function(&self.fn_wstr());
        code.function(&self.fn_witoa());
        code.function(&update);
        code.function(&patches);
        code.function(&boot);
        code.function(&init);
        code.function(&dispatch);
        module.section(&code);

        // Data: string constants.
        let mut data = DataSection::new();
        data.active(
            0,
            &ConstExpr::i32_const(DATA_BASE),
            self.pool.bytes.iter().copied(),
        );
        module.section(&data);

        Ok(module.finish())
    }

    /// w8(b): mem[cursor++] = b
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

    /// wvu(n): unsigned LEB128.
    fn fn_wvu(&self) -> Function {
        let mut f = Function::new([(1, ValType::I32)]); // local 1 = b
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
            I::Br(1), // continue the loop
            I::Else,
            I::LocalGet(1),
            I::Call(F_W8),
            I::End,
            I::End, // loop exits when the else branch ran
            I::End,
        ] {
            f.instruction(&i);
        }
        f
    }

    /// wstr(off, len): varint length + bytes copied from the constant pool.
    fn fn_wstr(&self) -> Function {
        let mut f = Function::new([]);
        for i in [
            I::LocalGet(1),
            I::Call(F_WVU),
            I::GlobalGet(G_CURSOR), // dst
            I::LocalGet(0),         // src
            I::LocalGet(1),         // size
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

    /// witoa(n): varint-length-prefixed decimal of an i64.
    /// (i64::MIN would render wrong through the negate; fine for a spike.)
    fn fn_witoa(&self) -> Function {
        let mut f = Function::new([(2, ValType::I32)]); // 1 = p, 2 = neg
        let mut ins: Vec<I<'static>> = Vec::new();
        // if n < 0 { neg = 1; n = 0 - n }
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
        ]);
        // do { scratch[p++] = '0' + n % 10; n /= 10 } while n != 0
        ins.extend([
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
        ]);
        // length byte (< 128 always for i64), optional '-', reversed digits.
        ins.extend([
            I::LocalGet(1),
            I::LocalGet(2),
            I::I32Add,
            I::Call(F_W8),
            I::LocalGet(2),
            I::If(BlockType::Empty),
            I::I32Const(45), // '-'
            I::Call(F_W8),
            I::End,
            I::Block(BlockType::Empty),
            I::Loop(BlockType::Empty),
            I::LocalGet(1),
            I::I32Eqz,
            I::BrIf(1),
            I::LocalGet(1),
            I::I32Const(1),
            I::I32Sub,
            I::LocalSet(1),
            I::I32Const(SCRATCH),
            I::LocalGet(1),
            I::I32Add,
            I::I32Load8U(MemArg {
                offset: 0,
                align: 0,
                memory_index: 0,
            }),
            I::Call(F_W8),
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

    /// update(msg): record-update semantics — new values computed against the
    /// pre-update model into locals, then struct.set.
    fn fn_update(&mut self) -> Result<Function, String> {
        let nfields = self.fields.len() as u32;
        let updates = self.app.updates.clone();
        let mut ins: Vec<I<'static>> = Vec::new();
        for u in &updates {
            let k = self.msg_index(&u.msg);
            ins.push(I::LocalGet(0));
            ins.push(I::I32Const(k as i32));
            ins.push(I::I32Eq);
            ins.push(I::If(BlockType::Empty));
            for (field, expr, _) in &u.fields {
                self.int_expr(expr, &mut ins)?;
                ins.push(I::LocalSet(1 + self.field_index(field)));
            }
            for (field, _, _) in &u.fields {
                let idx = self.field_index(field);
                ins.push(I::GlobalGet(G_STATE));
                ins.push(I::LocalGet(1 + idx));
                ins.push(I::StructSet {
                    struct_type_index: T_MODEL,
                    field_index: idx,
                });
            }
            ins.push(I::Return);
            ins.push(I::End);
        }
        ins.push(I::End);
        let mut f = Function::new([(nfields, ValType::I64)]);
        for i in &ins {
            f.instruction(i);
        }
        Ok(f)
    }

    /// patches(): the compile-time patch plan — one SetText per dynamic text,
    /// unconditionally re-serialized. (Caching last values so unchanged texts
    /// emit nothing is a later optimization; unconditional is protocol-correct.)
    fn fn_patches(&mut self) -> Result<Function, String> {
        let dyns: Vec<(Vec<u32>, Expr)> = self
            .dyns
            .iter()
            .map(|d| (d.path.clone(), d.expr.clone()))
            .collect();
        let mut ins: Vec<I<'static>> = Vec::new();
        Self::c_w8(&mut ins, WIRE_VERSION);
        Self::c_wvu(&mut ins, dyns.len() as u32);
        for (path, expr) in &dyns {
            Self::c_w8(&mut ins, 1); // setText
            Self::c_wvu(&mut ins, path.len() as u32);
            for seg in path {
                Self::c_wvu(&mut ins, *seg);
            }
            self.str_expr(expr, &mut ins)?;
        }
        for _ in 0..3 {
            Self::c_wvu(&mut ins, 0); // events, cmds, subs
        }
        ins.push(I::End);
        let mut f = Function::new([]);
        for i in &ins {
            f.instruction(i);
        }
        Ok(f)
    }

    /// boot(): model = struct.new(init values). Runs via the start section.
    fn fn_boot(&mut self) -> Result<Function, String> {
        let inits: Vec<Expr> = self
            .fields
            .iter()
            .map(|name| {
                self.app
                    .init
                    .iter()
                    .find(|(n, _, _)| n == name)
                    .map(|(_, e, _)| e.clone())
                    .expect("typechecked: init is total")
            })
            .collect();
        let mut ins: Vec<I<'static>> = Vec::new();
        for expr in &inits {
            self.int_expr(expr, &mut ins)?;
        }
        ins.push(I::StructNew(T_MODEL));
        ins.push(I::GlobalSet(G_STATE));
        ins.push(I::End);
        let mut f = Function::new([]);
        for i in &ins {
            f.instruction(i);
        }
        Ok(f)
    }

    /// init(): full InitialRender — version, tree, events, no cmds/subs.
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
        let mut f = Function::new([]);
        for i in &ins {
            f.instruction(i);
        }
        Ok(f)
    }

    /// dispatch(event_idx, path_len): deepest-first prefix match over the
    /// compile-time handler table (bubbling), then update + patch plan.
    fn fn_dispatch(&self) -> Function {
        let mut ins: Vec<I<'static>> = Vec::new();
        ins.push(I::I32Const(0));
        ins.push(I::GlobalSet(G_CURSOR));
        for h in &self.handlers {
            ins.push(I::Block(BlockType::Empty));
            ins.push(I::LocalGet(0));
            ins.push(I::I32Const(self.event_code(&h.event)));
            ins.push(I::I32Ne);
            ins.push(I::BrIf(0));
            ins.push(I::LocalGet(1));
            ins.push(I::I32Const(h.path.len() as i32));
            ins.push(I::I32LtU);
            ins.push(I::BrIf(0));
            for (i, seg) in h.path.iter().enumerate() {
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
            ins.push(I::I32Const(h.msg as i32));
            ins.push(I::Call(F_UPDATE));
            ins.push(I::Call(F_PATCHES));
            ins.push(I::GlobalGet(G_CURSOR));
            ins.push(I::Return);
            ins.push(I::End);
        }
        // No handler matched: empty update.
        Self::c_w8(&mut ins, WIRE_VERSION);
        for _ in 0..4 {
            Self::c_wvu(&mut ins, 0);
        }
        ins.push(I::GlobalGet(G_CURSOR));
        ins.push(I::End);
        let mut f = Function::new([]);
        for i in &ins {
            f.instruction(i);
        }
        f
    }
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

    const COUNTER: &str = r#"
app Counter
model { count: Int }
init = { count = 0 }
msg Inc | Dec | Reset
update Inc = { count = model.count + 1 }
update Dec = { count = model.count - 1 }
update Reset = { count = 0 }
view =
  div [class "counter"] [
    h1 [] [ text "zumar-lang" ],
    div [class "row"] [
      button [onClick Dec] [ text "-" ],
      span [class "count"] [ text show(model.count) ],
      button [onClick Inc] [ text "+" ]
    ],
    button [class "reset", onClick Reset] [ text "reset" ],
    p [class "note"] [ text (if model.count > 9 then "double digits!" else "") ]
  ]
"#;

    fn emit_counter() -> Vec<u8> {
        let app = zumar_lang::compile(COUNTER).unwrap();
        emit(&app).unwrap()
    }

    #[test]
    fn counter_emits_a_valid_gc_module() {
        let bytes = emit_counter();
        wasmparser::Validator::new()
            .validate_all(&bytes)
            .unwrap_or_else(|e| panic!("emitted module is invalid: {e}"));
        // Self-contained and small.
        assert!(
            bytes.len() < 4096,
            "module unexpectedly large: {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn unsupported_features_error_cleanly() {
        let todoish = r#"
app T
record Item { id: Int }
model { n: Int }
init = { n = 0 }
msg M
update M = { n = 1 }
view = div [] []
"#;
        let app = zumar_lang::compile(todoish).unwrap();
        let err = emit(&app).unwrap_err();
        assert!(err.contains("record types"), "{err}");
        assert!(err.contains("not yet in the wasmgc backend"), "{err}");
    }
}
