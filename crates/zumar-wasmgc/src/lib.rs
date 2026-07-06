#![forbid(unsafe_code)]
//! WasmGC backend: emit a self-contained GC module from the zumar-lang AST
//! via `wasm-encoder`. No Rust toolchain, no wasm-bindgen, no runtime crate —
//! the module is the whole app.
//!
//! The load-bearing idea: `.zu` views are statically known, so the compiler
//! emits a **compile-time patch plan** instead of shipping a diff. Every
//! dynamic text node and attribute has a compile-time path; `dispatch` runs
//! the update against the GC model struct and re-serializes exactly those as
//! SetText/SetAttr patches. No vdom in memory, no diff at runtime.
//!
//! Strings live on the GC heap as `array (mut i8)`. String expressions are
//! *values* (constants via `array.new_data` from a passive segment, payload
//! via a staging buffer, `show` via an emitted itoa, `++` via an emitted
//! concat), written to the wire by one serializer. Int model fields are i64
//! struct fields.
//!
//! Boundary (raw exports, no glue; `www/zumar-gc.js` adapts this to the
//! standard shim):
//! - `init() -> len` — wire-encoded InitialRender at mem[0..len]
//! - `dispatch(event_idx, path_len, payload_len) -> len` — event_idx indexes
//!   the events array of the init message; the host writes the path's u32s
//!   at `path_buf` and any String payload's UTF-8 at `payload_buf` first.
//! - `mem`, `path_buf`, `payload_buf` exports.
//!
//! Subset today: Int and String model fields, payload-less and
//! String-payload messages, `onClick`/`onChange`/`onSubmit`/`onInput`,
//! static element structure, dynamic text and attributes over Int/String
//! expressions. Lists, records, `Maybe`, Bool payloads, and effects error
//! with a pointer back to the default Rust backend.

use std::collections::BTreeMap;

use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, DataCountSection, DataSection, ExportKind, ExportSection,
    FieldType, Function, FunctionSection, GlobalSection, GlobalType, HeapType, Instruction as I,
    MemArg, MemorySection, MemoryType, Module, RefType, StartSection, StorageType, TypeSection,
    ValType,
};
use zumar_lang::ast::{App, Attr, Child, Element, Expr, Op, Pos, Ty, ValueKind};

const WIRE_VERSION: u8 = 1;

// Memory layout: wire buffer grows from 0; itoa scratch; string constants
// (active data segment); payload and path staging written by the host.
const SCRATCH: i32 = 3072;
const DATA_BASE: i32 = 4096;
const PAYLOAD_BUF: i32 = 56000;
const PATH_BUF: i32 = 60000;

// Type indices.
const T_STR: u32 = 0; // array (mut i8)
const T_MODEL: u32 = 1;
const T_I32: u32 = 2; // (i32) -> ()
const T_II: u32 = 3; // (i32, i32) -> ()
const T_VOID: u32 = 4; // () -> ()
const T_RET: u32 = 5; // () -> i32
const T_DISPATCH: u32 = 6; // (i32, i32, i32) -> i32
const T_ITOA: u32 = 7; // (i64) -> str
const T_MEM2STR: u32 = 8; // (i32, i32) -> str
const T_CONCAT: u32 = 9; // (str, str) -> str
const T_STREQ: u32 = 10; // (str, str) -> i32
const T_WSTRGC: u32 = 11; // (str) -> ()

// Function indices (order of bodies below).
const F_W8: u32 = 0;
const F_WVU: u32 = 1;
const F_WSTR: u32 = 2;
const F_ITOA: u32 = 3;
const F_MEM2STR: u32 = 4;
const F_CONCAT: u32 = 5;
const F_STREQ: u32 = 6;
const F_WSTRGC: u32 = 7;
const F_UPDATE: u32 = 8;
const F_PATCHES: u32 = 9;
const F_BOOT: u32 = 10;
const F_INIT: u32 = 11;
const F_DISPATCH: u32 = 12;

// Globals.
const G_STATE: u32 = 0;
const G_CURSOR: u32 = 1;
const G_PATH_BUF: u32 = 2;
const G_PAYLOAD_BUF: u32 = 3;
const G_PAYLOAD: u32 = 4;

fn str_ref() -> RefType {
    RefType {
        nullable: true,
        heap_type: HeapType::Concrete(T_STR),
    }
}

fn str_val() -> ValType {
    ValType::Ref(str_ref())
}

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

struct DynText {
    path: Vec<u32>,
    expr: Expr,
}

struct DynAttr {
    path: Vec<u32>,
    name: String,
    expr: Expr,
}

struct Handler {
    path: Vec<u32>,
    event: String,
    msg: u32,
    takes_payload: bool,
}

struct Emitter<'a> {
    app: &'a App,
    fields: Vec<(String, Ty)>,
    msgs: Vec<String>,
    pool: Pool,
    tree: Vec<I<'static>>,
    dyn_texts: Vec<DynText>,
    dyn_attrs: Vec<DynAttr>,
    handlers: Vec<Handler>,
    events: BTreeMap<String, bool>,
    /// The bound payload variable while emitting an update arm.
    payload_var: Option<String>,
}

impl<'a> Emitter<'a> {
    fn build(app: &'a App) -> Result<Emitter<'a>, String> {
        if let Some(r) = app.records.first() {
            return Err(unsupported(r.pos, "record types"));
        }
        for (name, ty, pos) in &app.model {
            if !matches!(ty, Ty::Int | Ty::Str) {
                return Err(unsupported(*pos, &format!("model field `{name}: {ty}`")));
            }
        }
        for m in &app.msgs {
            match &m.payload {
                None | Some(Ty::Str) => {}
                Some(other) => {
                    return Err(unsupported(m.pos, &format!("`{other}` message payloads")))
                }
            }
        }
        let mut e = Emitter {
            app,
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
            handlers: Vec::new(),
            events: BTreeMap::new(),
            payload_var: None,
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

    fn msg_index(&self, name: &str) -> u32 {
        self.msgs
            .iter()
            .position(|m| m == name)
            .expect("typechecked") as u32
    }

    fn msg_takes_payload(&self, index: u32) -> bool {
        self.app.msgs[index as usize].payload.is_some()
    }

    fn event_code(&self, name: &str) -> i32 {
        self.events
            .keys()
            .position(|e| e == name)
            .expect("collected") as i32
    }

    /// Minimal type resolution for the supported subset (the checker has
    /// already validated the program; this only steers codegen).
    fn ty_of(&self, e: &Expr) -> Ty {
        match e {
            Expr::Int(_) | Expr::ToInt(..) | Expr::Len(..) | Expr::Sum(..) => Ty::Int,
            Expr::Str(_) | Expr::Show(..) => Ty::Str,
            Expr::Bool(_) | Expr::Not(..) => Ty::Bool,
            Expr::Var(v, _) if self.payload_var.as_deref() == Some(v) => Ty::Str,
            Expr::Field(_, f, _) => self.field_ty(f),
            Expr::Bin(Op::Concat, ..) => Ty::Str,
            Expr::Bin(Op::Add | Op::Sub | Op::Mul, ..) => Ty::Int,
            Expr::Bin(..) => Ty::Bool,
            Expr::If(_, t, ..) => self.ty_of(t),
            _ => Ty::Int,
        }
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

    /// Write a pool string (varint length + bytes) from linear memory.
    fn c_wstr(&mut self, out: &mut Vec<I<'static>>, s: &str) {
        let (off, len) = self.pool.intern(s);
        out.push(I::I32Const(DATA_BASE + off));
        out.push(I::I32Const(len));
        out.push(I::Call(F_WSTR));
    }

    /// Write a text expression: constants stream from the pool, dynamic
    /// expressions build a GC string and serialize it.
    fn c_text(&mut self, expr: &Expr, out: &mut Vec<I<'static>>) -> Result<(), String> {
        if let Expr::Str(s) = expr {
            let s = s.clone();
            self.c_wstr(out, &s);
            return Ok(());
        }
        self.str_expr(expr, out)?;
        out.push(I::Call(F_WSTRGC));
        Ok(())
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
                    if !matches!(value, Expr::Str(_)) {
                        self.dyn_attrs.push(DynAttr {
                            path: path.clone(),
                            name: name.clone(),
                            expr: value.clone(),
                        });
                    }
                    str_attrs.push((name.clone(), value.clone()));
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
                        takes_payload: false,
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
                        path: path.clone(),
                        event: event.clone(),
                        msg,
                        takes_payload: true,
                    });
                }
            }
        }
        Self::c_wvu(out, str_attrs.len() as u32);
        for (name, value) in str_attrs {
            self.c_wstr(out, &name);
            self.c_text(&value, out)?;
        }

        Self::c_wvu(out, el.children.len() as u32);
        for (i, child) in el.children.iter().enumerate() {
            path.push(i as u32);
            match child {
                Child::Elem(e) => self.walk(e, path, out)?,
                Child::Text(expr, _) => {
                    Self::c_w8(out, 0); // text node
                    if !matches!(expr, Expr::Str(_)) {
                        self.dyn_texts.push(DynText {
                            path: path.clone(),
                            expr: expr.clone(),
                        });
                    }
                    let expr = expr.clone();
                    self.c_text(&expr, out)?;
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
                self.model_field(base, field, *pos, out)?;
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

    /// Push a `(ref null $str)` onto the wasm stack.
    fn str_expr(&mut self, e: &Expr, out: &mut Vec<I<'static>>) -> Result<(), String> {
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
            Expr::Var(v, pos) => {
                if self.payload_var.as_deref() == Some(v) {
                    out.push(I::GlobalGet(G_PAYLOAD));
                } else {
                    return Err(unsupported(*pos, &format!("variable `{v}`")));
                }
            }
            Expr::Field(base, field, pos) => {
                self.model_field(base, field, *pos, out)?;
            }
            Expr::Show(inner, _) => {
                self.int_expr(inner, out)?;
                out.push(I::Call(F_ITOA));
            }
            Expr::Bin(Op::Concat, l, r, _) => {
                self.str_expr(l, out)?;
                self.str_expr(r, out)?;
                out.push(I::Call(F_CONCAT));
            }
            Expr::If(c, t, f, _) => {
                self.bool_expr(c, out)?;
                out.push(I::If(BlockType::Result(str_val())));
                self.str_expr(t, out)?;
                out.push(I::Else);
                self.str_expr(f, out)?;
                out.push(I::End);
            }
            other => return Err(unsupported(pos_of(other), "this text expression")),
        }
        Ok(())
    }

    /// `model.<field>` -> struct.get (works for both i64 and str fields).
    fn model_field(
        &mut self,
        base: &Expr,
        field: &str,
        pos: Pos,
        out: &mut Vec<I<'static>>,
    ) -> Result<(), String> {
        let Expr::Var(m, _) = base else {
            return Err(unsupported(pos, "nested field access"));
        };
        if m != "model" {
            return Err(unsupported(pos, "variables other than `model`"));
        }
        out.push(I::GlobalGet(G_STATE));
        out.push(I::StructGet {
            struct_type_index: T_MODEL,
            field_index: self.field_index(field),
        });
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
            Expr::Bin(op @ (Op::Eq | Op::Ne), l, r, _) => {
                if self.ty_of(l) == Ty::Str {
                    self.str_expr(l, out)?;
                    self.str_expr(r, out)?;
                    out.push(I::Call(F_STREQ));
                    if *op == Op::Ne {
                        out.push(I::I32Eqz);
                    }
                } else {
                    self.int_expr(l, out)?;
                    self.int_expr(r, out)?;
                    out.push(if *op == Op::Eq { I::I64Eq } else { I::I64Ne });
                }
            }
            Expr::Bin(op @ (Op::Lt | Op::Gt), l, r, _) => {
                self.int_expr(l, out)?;
                self.int_expr(r, out)?;
                out.push(if *op == Op::Lt { I::I64LtS } else { I::I64GtS });
            }
            other => return Err(unsupported(pos_of(other), "this condition")),
        }
        Ok(())
    }

    /// A model-field value by type (used by update arms and boot).
    fn val_expr(&mut self, e: &Expr, ty: &Ty, out: &mut Vec<I<'static>>) -> Result<(), String> {
        match ty {
            Ty::Int => self.int_expr(e, out),
            Ty::Str => self.str_expr(e, out),
            _ => Err(unsupported(pos_of(e), "this field type")),
        }
    }

    // --- module assembly -----------------------------------------------------

    fn module(mut self) -> Result<Vec<u8>, String> {
        let mut module = Module::new();

        // Types. $str first so the model struct can reference it.
        let mut types = TypeSection::new();
        types.ty().array(&StorageType::I8, true);
        types.ty().struct_(
            self.fields
                .iter()
                .map(|(_, t)| FieldType {
                    element_type: StorageType::Val(match t {
                        Ty::Int => ValType::I64,
                        _ => str_val(),
                    }),
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
        module.section(&types);

        // Functions.
        let mut funcs = FunctionSection::new();
        for ty in [
            T_I32, T_I32, T_II, T_ITOA, T_MEM2STR, T_CONCAT, T_STREQ, T_WSTRGC, T_I32, T_VOID,
            T_VOID, T_RET, T_DISPATCH,
        ] {
            funcs.function(ty);
        }
        module.section(&funcs);

        // Memory: 2 pages (wire buffer + constants + staging).
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
                val_type: ValType::Ref(RefType {
                    nullable: true,
                    heap_type: HeapType::Concrete(T_MODEL),
                }),
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
        module.section(&globals);

        // Exports.
        let mut exports = ExportSection::new();
        exports.export("mem", ExportKind::Memory, 0);
        exports.export("init", ExportKind::Func, F_INIT);
        exports.export("dispatch", ExportKind::Func, F_DISPATCH);
        exports.export("path_buf", ExportKind::Global, G_PATH_BUF);
        exports.export("payload_buf", ExportKind::Global, G_PAYLOAD_BUF);
        module.section(&exports);

        module.section(&StartSection {
            function_index: F_BOOT,
        });

        // array.new_data requires a data-count section before code.
        module.section(&DataCountSection { count: 2 });

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
        code.function(&self.fn_itoa());
        code.function(&self.fn_mem2str());
        code.function(&self.fn_concat());
        code.function(&self.fn_streq());
        code.function(&self.fn_wstrgc());
        code.function(&update);
        code.function(&patches);
        code.function(&boot);
        code.function(&init);
        code.function(&dispatch);
        module.section(&code);

        // Data: segment 0 active (streamed by wstr via memory.copy),
        // segment 1 passive (materialized by array.new_data). Same bytes.
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

    /// wstr(addr, len): varint length + bytes copied from linear memory.
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

    /// itoa(n) -> str: decimal digits of an i64 as a fresh GC string.
    /// (i64::MIN renders wrong through the negate; acceptable for now.)
    fn fn_itoa(&self) -> Function {
        // params: 0 = n (i64); locals: 1 = p, 2 = neg (i32), 3 = s, 4 = i
        let mut f = Function::new([(2, ValType::I32), (1, str_val()), (1, ValType::I32)]);
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
        // s = new array(p + neg); s[0] = '-' when negative.
        ins.extend([
            I::LocalGet(1),
            I::LocalGet(2),
            I::I32Add,
            I::ArrayNewDefault(T_STR),
            I::LocalSet(3),
            I::LocalGet(2),
            I::If(BlockType::Empty),
            I::LocalGet(3),
            I::I32Const(0),
            I::I32Const(45), // '-'
            I::ArraySet(T_STR),
            I::End,
        ]);
        // for i in 0..p: s[neg + i] = scratch[p - 1 - i]
        ins.extend([
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

    /// mem2str(addr, len) -> str: build a GC string from linear memory
    /// (the payload staging buffer).
    fn fn_mem2str(&self) -> Function {
        // params: 0 = addr, 1 = len; locals: 2 = i, 3 = s
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

    /// concat(a, b) -> str.
    fn fn_concat(&self) -> Function {
        // params: 0 = a, 1 = b; locals: 2 = la, 3 = lb, 4 = s
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

    /// streq(a, b) -> i32.
    fn fn_streq(&self) -> Function {
        // params: 0 = a, 1 = b; locals: 2 = i, 3 = la
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

    /// wstrgc(s): varint length + the GC string's bytes into the wire buffer.
    fn fn_wstrgc(&self) -> Function {
        // params: 0 = s; locals: 1 = i, 2 = l
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

    /// update(msg): record-update semantics — new values computed against the
    /// pre-update model into locals, then struct.set. A message with a String
    /// payload reads it from the payload global (set by dispatch).
    fn fn_update(&mut self) -> Result<Function, String> {
        let updates = self.app.updates.clone();
        let mut ins: Vec<I<'static>> = Vec::new();
        for u in &updates {
            let k = self.msg_index(&u.msg);
            self.payload_var = u.var.as_ref().map(|(v, _)| v.clone());
            ins.push(I::LocalGet(0));
            ins.push(I::I32Const(k as i32));
            ins.push(I::I32Eq);
            ins.push(I::If(BlockType::Empty));
            for (field, expr, _) in &u.fields {
                let ty = self.field_ty(field);
                self.val_expr(expr, &ty, &mut ins)?;
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
            self.payload_var = None;
        }
        ins.push(I::End);
        // One local per model field, typed to match.
        let locals: Vec<(u32, ValType)> = self
            .fields
            .iter()
            .map(|(_, t)| {
                (
                    1u32,
                    if *t == Ty::Int {
                        ValType::I64
                    } else {
                        str_val()
                    },
                )
            })
            .collect();
        let mut f = Function::new(locals);
        for i in &ins {
            f.instruction(i);
        }
        Ok(f)
    }

    /// patches(): the compile-time patch plan — SetText for each dynamic
    /// text, SetAttr for each dynamic attribute, unconditionally
    /// re-serialized. (Caching last values is a later optimization.)
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
        let mut ins: Vec<I<'static>> = Vec::new();
        Self::c_w8(&mut ins, WIRE_VERSION);
        Self::c_wvu(&mut ins, (texts.len() + attrs.len()) as u32);
        for (path, expr) in &texts {
            Self::c_w8(&mut ins, 1); // setText
            Self::c_wvu(&mut ins, path.len() as u32);
            for seg in path {
                Self::c_wvu(&mut ins, *seg);
            }
            self.c_text(expr, &mut ins)?;
        }
        for (path, name, expr) in &attrs {
            Self::c_w8(&mut ins, 2); // setAttr
            Self::c_wvu(&mut ins, path.len() as u32);
            for seg in path {
                Self::c_wvu(&mut ins, *seg);
            }
            self.c_wstr(&mut ins, name);
            self.c_text(expr, &mut ins)?;
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
        let mut ins: Vec<I<'static>> = Vec::new();
        for (expr, ty) in &inits {
            self.val_expr(expr, ty, &mut ins)?;
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

    /// dispatch(event_idx, path_len, payload_len): deepest-first prefix match
    /// over the compile-time handler table (bubbling); payload-taking
    /// handlers materialize the staged bytes as a GC string first.
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
            if h.takes_payload && self.msg_takes_payload(h.msg) {
                ins.push(I::I32Const(PAYLOAD_BUF));
                ins.push(I::LocalGet(2));
                ins.push(I::Call(F_MEM2STR));
                ins.push(I::GlobalSet(G_PAYLOAD));
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

    const HELLO: &str = r#"
app Hello
model { name: String, taps: Int }
init = { name = "", taps = 0 }
msg Name String | Clear
update Name s = { name = s }
update Clear = { name = "", taps = model.taps + 1 }
view =
  div [] [
    input [type "text", value model.name, onInput Name] [],
    p [] [ text (if model.name == "" then "?" else "kaixo, " ++ model.name) ],
    button [onClick Clear] [ text "clear" ]
  ]
"#;

    fn emit_src(src: &str) -> Result<Vec<u8>, String> {
        emit(&zumar_lang::compile(src).unwrap())
    }

    #[test]
    fn counter_emits_a_valid_gc_module() {
        let bytes = emit_src(COUNTER).unwrap();
        wasmparser::Validator::new()
            .validate_all(&bytes)
            .unwrap_or_else(|e| panic!("emitted module is invalid: {e}"));
        assert!(
            bytes.len() < 8192,
            "module unexpectedly large: {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn hello_with_gc_strings_emits_a_valid_module() {
        let bytes = emit_src(HELLO).unwrap();
        wasmparser::Validator::new()
            .validate_all(&bytes)
            .unwrap_or_else(|e| panic!("emitted module is invalid: {e}"));
    }

    #[test]
    fn unsupported_features_error_cleanly() {
        let src = r#"
app T
record Item { id: Int }
model { n: Int }
init = { n = 0 }
msg M
update M = { n = 1 }
view = div [] []
"#;
        let app = zumar_lang::compile(src).unwrap();
        let err = emit(&app).unwrap_err();
        assert!(err.contains("record types"), "{err}");
        assert!(err.contains("not yet in the wasmgc backend"), "{err}");
    }

    #[test]
    fn bool_payloads_error_cleanly() {
        let src = r#"
app B
model { on: Bool }
init = { on = false }
msg Flip Bool
update Flip b = { on = b }
view = div [] [ input [type "checkbox", onCheck Flip] [] ]
"#;
        let app = zumar_lang::compile(src).unwrap();
        let err = emit(&app).unwrap_err();
        assert!(err.contains("not yet in the wasmgc backend"), "{err}");
    }
}
