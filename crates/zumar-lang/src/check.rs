//! Type checking for zumar-lang.
//!
//! Monomorphic and total: `init` must build the whole model, every declared
//! message must have exactly one `update`, and every expression resolves to
//! one concrete type. Records, `List`, message payloads, and comprehensions
//! are checked with a scoped environment (`model`, the payload variable, and
//! loop variables). The Elm guarantee — no click can hit a hole — is the
//! totality check at the bottom.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::*;

type Fields = Vec<(String, Ty)>;
/// A lexical scope: innermost bindings last.
type Env = Vec<(String, Ty)>;

pub fn check(app: &App) -> Result<(), ZuError> {
    Checker::build(app)?.run(app)
}

struct Checker {
    records: BTreeMap<String, Fields>,
    msgs: BTreeMap<String, Option<Ty>>,
}

impl Checker {
    fn build(app: &App) -> Result<Checker, ZuError> {
        let mut records: BTreeMap<String, Fields> = BTreeMap::new();

        for r in &app.records {
            if r.name == MODEL {
                return Err(ZuError::at(
                    r.pos,
                    "`Model` is reserved for the model block",
                ));
            }
            let mut fields = Vec::new();
            let mut seen = BTreeSet::new();
            for (name, ty, pos) in &r.fields {
                if !seen.insert(name.clone()) {
                    return Err(ZuError::at(
                        *pos,
                        format!("duplicate field `{name}` in `{}`", r.name),
                    ));
                }
                fields.push((name.clone(), ty.clone()));
            }
            if records.insert(r.name.clone(), fields).is_some() {
                return Err(ZuError::at(r.pos, format!("duplicate record `{}`", r.name)));
            }
        }

        let mut model_fields = Vec::new();
        let mut seen = BTreeSet::new();
        for (name, ty, pos) in &app.model {
            if !seen.insert(name.clone()) {
                return Err(ZuError::at(*pos, format!("duplicate model field `{name}`")));
            }
            model_fields.push((name.clone(), ty.clone()));
        }
        records.insert(MODEL.into(), model_fields);

        // Every referenced record must exist (after all names are known).
        let names: BTreeSet<String> = records.keys().cloned().collect();
        for r in &app.records {
            for (_, ty, pos) in &r.fields {
                check_ty_refs(ty, &names, *pos)?;
            }
        }
        for (name, ty, pos) in &app.model {
            let _ = name;
            check_ty_refs(ty, &names, *pos)?;
        }

        let mut msgs = BTreeMap::new();
        for m in &app.msgs {
            if let Some(ty) = &m.payload {
                check_ty_refs(ty, &names, m.pos)?;
            }
            if msgs.insert(m.name.clone(), m.payload.clone()).is_some() {
                return Err(ZuError::at(
                    m.pos,
                    format!("duplicate message `{}`", m.name),
                ));
            }
        }

        Ok(Checker { records, msgs })
    }

    fn model(&self) -> &Fields {
        &self.records[MODEL]
    }

    fn field_ty(&self, record: &str, field: &str) -> Option<&Ty> {
        self.records
            .get(record)?
            .iter()
            .find(|(n, _)| n == field)
            .map(|(_, t)| t)
    }

    fn run(&self, app: &App) -> Result<(), ZuError> {
        // init: every model field, correct type, no strays. Empty env — init
        // is a constant, it can't read `model`.
        let mut seen = BTreeSet::new();
        for (name, expr, pos) in &app.init {
            let Some(want) = self.field_ty(MODEL, name).cloned() else {
                return Err(ZuError::at(
                    *pos,
                    format!("`init` sets unknown field `{name}`"),
                ));
            };
            if !seen.insert(name.clone()) {
                return Err(ZuError::at(*pos, format!("`init` sets `{name}` twice")));
            }
            self.expect(expr, &want, &Env::new(), &format!("init field `{name}`"))?;
        }
        for (name, _) in self.model() {
            if !seen.contains(name) {
                return Err(ZuError::at(
                    app.init
                        .first()
                        .map(|(_, _, p)| *p)
                        .unwrap_or(Pos { line: 1, col: 1 }),
                    format!("`init` is missing field `{name}`"),
                ));
            }
        }

        // updates: known msg, payload var matches, model fields, right types.
        let mut handled = BTreeSet::new();
        for u in &app.updates {
            let Some(payload) = self.msgs.get(&u.msg) else {
                return Err(ZuError::at(
                    u.pos,
                    format!("`update {}` refers to an undeclared message", u.msg),
                ));
            };
            if !handled.insert(u.msg.clone()) {
                return Err(ZuError::at(
                    u.pos,
                    format!("duplicate `update {}` equation", u.msg),
                ));
            }
            let mut env: Env = vec![(String::from("model"), Ty::Record(MODEL.into()))];
            match (payload, &u.var) {
                (Some(ty), Some((var, _))) => env.push((var.clone(), ty.clone())),
                (Some(ty), None) => {
                    return Err(ZuError::at(
                        u.pos,
                        format!(
                        "`update {}` must bind the payload: `update {} x = ...` (payload is {ty})",
                        u.msg, u.msg
                    ),
                    ))
                }
                (None, Some((_, vpos))) => {
                    return Err(ZuError::at(
                        *vpos,
                        format!("message `{}` carries no payload", u.msg),
                    ))
                }
                (None, None) => {}
            }
            let mut set = BTreeSet::new();
            for (name, expr, pos) in &u.fields {
                let Some(want) = self.field_ty(MODEL, name).cloned() else {
                    return Err(ZuError::at(
                        *pos,
                        format!("`update {}` sets unknown field `{name}`", u.msg),
                    ));
                };
                if !set.insert(name.clone()) {
                    return Err(ZuError::at(
                        *pos,
                        format!("`update {}` sets `{name}` twice", u.msg),
                    ));
                }
                self.expect(
                    expr,
                    &want,
                    &env,
                    &format!("`update {}`, field `{name}`", u.msg),
                )?;
            }
        }
        for m in &app.msgs {
            if !handled.contains(&m.name) {
                return Err(ZuError::at(
                    m.pos,
                    format!("message `{}` has no `update {} = ...` equation — every message must be handled", m.name, m.name),
                ));
            }
        }

        let env: Env = vec![(String::from("model"), Ty::Record(MODEL.into()))];
        self.check_element(&app.view, &env)
    }

    fn check_element(&self, el: &Element, env: &Env) -> Result<(), ZuError> {
        for attr in &el.attrs {
            match attr {
                Attr::Str { name, value, pos } => {
                    let _ = pos;
                    self.expect(value, &Ty::Str, env, &format!("attribute `{name}`"))?;
                }
                Attr::On { handler, .. } => self.check_msg_call(handler, env)?,
                Attr::OnValue {
                    event: _,
                    ctor,
                    kind,
                    pos,
                } => {
                    let want = match kind {
                        ValueKind::Value => Ty::Str,
                        ValueKind::Checked => Ty::Bool,
                    };
                    match self.msgs.get(ctor) {
                        None => return Err(ZuError::at(*pos, format!("`{ctor}` is not a declared message"))),
                        Some(Some(p)) if *p == want => {}
                        Some(_) => {
                            return Err(ZuError::at(
                                *pos,
                                format!("this handler feeds a {want} to `{ctor}`, but `{ctor}` doesn't take a {want} payload"),
                            ))
                        }
                    }
                }
            }
        }
        for child in &el.children {
            match child {
                Child::Elem(e) => self.check_element(e, env)?,
                Child::Text(expr, _) => self.expect(expr, &Ty::Str, env, "`text`")?,
                Child::For {
                    var,
                    list,
                    body,
                    pos,
                } => {
                    let elem = self.list_elem(list, env, *pos)?;
                    let mut inner = env.clone();
                    inner.push((var.clone(), elem));
                    self.check_element(body, &inner)?;
                }
            }
        }
        Ok(())
    }

    fn check_msg_call(&self, call: &MsgCall, env: &Env) -> Result<(), ZuError> {
        let Some(payload) = self.msgs.get(&call.name) else {
            return Err(ZuError::at(
                call.pos,
                format!("`{}` is not a declared message", call.name),
            ));
        };
        match (payload, &call.arg) {
            (Some(ty), Some(arg)) => {
                self.expect(arg, ty, env, &format!("argument to `{}`", call.name))
            }
            (Some(ty), None) => Err(ZuError::at(
                call.pos,
                format!(
                    "`{}` needs a {ty} argument: `{}(...)`",
                    call.name, call.name
                ),
            )),
            (None, Some(_)) => Err(ZuError::at(
                call.pos,
                format!("`{}` takes no argument", call.name),
            )),
            (None, None) => Ok(()),
        }
    }

    /// The element type of a `List` expression, or an error at `pos`.
    fn list_elem(&self, list: &Expr, env: &Env, pos: Pos) -> Result<Ty, ZuError> {
        match self.infer(list, None, env)? {
            Ty::List(t) => Ok(*t),
            other => Err(ZuError::at(
                pos,
                format!("`for ... in` needs a List, got {other}"),
            )),
        }
    }

    fn expect(&self, expr: &Expr, want: &Ty, env: &Env, ctx: &str) -> Result<(), ZuError> {
        let got = self.infer(expr, Some(want), env)?;
        if &got != want {
            return Err(ZuError::at(
                pos_of(expr),
                format!("{ctx} expects {want}, but this is {got}"),
            ));
        }
        Ok(())
    }

    /// Infer a type. `expected` disambiguates empty list literals and record
    /// literals, and flows into `if` branches and comprehension bodies.
    fn infer(&self, expr: &Expr, expected: Option<&Ty>, env: &Env) -> Result<Ty, ZuError> {
        match expr {
            Expr::Int(_) => Ok(Ty::Int),
            Expr::Str(_) => Ok(Ty::Str),
            Expr::Bool(_) => Ok(Ty::Bool),
            Expr::Var(name, pos) => env
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .map(|(_, t)| t.clone())
                .ok_or_else(|| ZuError::at(*pos, format!("`{name}` is not in scope"))),
            Expr::Field(base, field, pos) => {
                let bt = self.infer(base, None, env)?;
                let Ty::Record(rec) = &bt else {
                    return Err(ZuError::at(
                        *pos,
                        format!("`{field}` needs a record on the left, got {bt}"),
                    ));
                };
                self.field_ty(rec, field)
                    .cloned()
                    .ok_or_else(|| ZuError::at(*pos, format!("{rec} has no field `{field}`")))
            }
            Expr::Show(inner, pos) => {
                self.expect_ty(inner, &Ty::Int, env, *pos, "show(..) takes an Int")?;
                Ok(Ty::Str)
            }
            Expr::Len(inner, pos) => match self.infer(inner, None, env)? {
                Ty::List(_) => Ok(Ty::Int),
                other => Err(ZuError::at(
                    *pos,
                    format!("length(..) takes a List, got {other}"),
                )),
            },
            Expr::Sum(inner, pos) => match self.infer(inner, None, env)? {
                Ty::List(t) if *t == Ty::Int => Ok(Ty::Int),
                other => Err(ZuError::at(
                    *pos,
                    format!("sum(..) takes a List Int, got {other}"),
                )),
            },
            Expr::ToInt(inner, pos) => {
                self.expect_ty(inner, &Ty::Str, env, *pos, "toInt(..) takes a String")?;
                Ok(Ty::Int)
            }
            Expr::Nth(list, index, default, pos) => {
                let elem = match self.infer(list, None, env)? {
                    Ty::List(t) => *t,
                    other => {
                        return Err(ZuError::at(
                            *pos,
                            format!("nth(..) takes a List, got {other}"),
                        ))
                    }
                };
                self.expect_ty(index, &Ty::Int, env, *pos, "nth index must be an Int")?;
                self.expect(default, &elem, env, "nth default")?;
                Ok(elem)
            }
            Expr::Reverse(inner, pos) => match self.infer(inner, None, env)? {
                Ty::List(t) => Ok(Ty::List(t)),
                other => Err(ZuError::at(
                    *pos,
                    format!("reverse(..) takes a List, got {other}"),
                )),
            },
            Expr::Not(inner, pos) => {
                self.expect_ty(inner, &Ty::Bool, env, *pos, "`not` takes a Bool")?;
                Ok(Ty::Bool)
            }
            Expr::Bin(op, l, r, pos) => self.infer_bin(*op, l, r, *pos, env),
            Expr::If(c, t, e, pos) => {
                self.expect_ty(c, &Ty::Bool, env, *pos, "`if` condition must be Bool")?;
                let tt = self.infer(t, expected, env)?;
                let et = self.infer(e, expected, env)?;
                if tt != et {
                    return Err(ZuError::at(
                        *pos,
                        format!("`if` branches disagree: then is {tt}, else is {et}"),
                    ));
                }
                Ok(tt)
            }
            Expr::ListLit(items, pos) => {
                let elem_hint = match expected {
                    Some(Ty::List(t)) => Some(t.as_ref()),
                    _ => None,
                };
                let mut iter = items.iter();
                let Some(first) = iter.next() else {
                    return match elem_hint {
                        Some(t) => Ok(Ty::List(Box::new(t.clone()))),
                        None => Err(ZuError::at(*pos, "can't infer the type of an empty list here — annotate the field or init")),
                    };
                };
                let elem = self.infer(first, elem_hint, env)?;
                for item in iter {
                    self.expect(item, &elem, env, "list element")?;
                }
                Ok(Ty::List(Box::new(elem)))
            }
            Expr::RecordLit(fields, pos) => {
                let name = self.resolve_record(fields, expected, *pos)?;
                self.check_record_fields(&name, fields, env, *pos)?;
                Ok(Ty::Record(name))
            }
            Expr::RecordUpdate(base, fields, pos) => {
                let bt = self.infer(base, expected, env)?;
                let Ty::Record(rec) = &bt else {
                    return Err(ZuError::at(
                        *pos,
                        format!("record update needs a record base, got {bt}"),
                    ));
                };
                for (fname, fexpr, fpos) in fields {
                    let Some(want) = self.field_ty(rec, fname).cloned() else {
                        return Err(ZuError::at(*fpos, format!("{rec} has no field `{fname}`")));
                    };
                    self.expect(fexpr, &want, env, &format!("field `{fname}`"))?;
                }
                Ok(bt)
            }
            Expr::For {
                var,
                list,
                cond,
                body,
                pos,
            } => {
                let elem = self.list_elem(list, env, *pos)?;
                let mut inner = env.clone();
                inner.push((var.clone(), elem));
                if let Some(c) = cond {
                    self.expect_ty(c, &Ty::Bool, &inner, *pos, "`where` clause must be Bool")?;
                }
                let body_hint = match expected {
                    Some(Ty::List(t)) => Some(t.as_ref()),
                    _ => None,
                };
                let bt = self.infer(body, body_hint, &inner)?;
                Ok(Ty::List(Box::new(bt)))
            }
        }
    }

    fn infer_bin(&self, op: Op, l: &Expr, r: &Expr, pos: Pos, env: &Env) -> Result<Ty, ZuError> {
        let lt = self.infer(l, None, env)?;
        let rt = self.infer(r, None, env)?;
        match op {
            Op::Add | Op::Sub | Op::Mul => {
                if lt != Ty::Int || rt != Ty::Int {
                    return Err(ZuError::at(
                        pos,
                        format!("arithmetic needs Int on both sides, got {lt} and {rt}"),
                    ));
                }
                Ok(Ty::Int)
            }
            Op::Concat => match (&lt, &rt) {
                (Ty::Str, Ty::Str) => Ok(Ty::Str),
                (Ty::List(a), Ty::List(b)) if a == b => Ok(Ty::List(a.clone())),
                _ => Err(ZuError::at(
                    pos,
                    format!("`++` joins two Strings or two matching Lists, got {lt} and {rt}"),
                )),
            },
            Op::Eq | Op::Ne => {
                if lt != rt {
                    return Err(ZuError::at(
                        pos,
                        format!("`==`/`!=` compare equal types, got {lt} and {rt}"),
                    ));
                }
                if matches!(lt, Ty::Record(_) | Ty::List(_)) {
                    return Err(ZuError::at(
                        pos,
                        format!("`==`/`!=` only compare Int/String/Bool, got {lt}"),
                    ));
                }
                Ok(Ty::Bool)
            }
            Op::Lt | Op::Gt => {
                if lt != Ty::Int || rt != Ty::Int {
                    return Err(ZuError::at(
                        pos,
                        format!("`<`/`>` compare Ints, got {lt} and {rt}"),
                    ));
                }
                Ok(Ty::Bool)
            }
        }
    }

    fn expect_ty(
        &self,
        expr: &Expr,
        want: &Ty,
        env: &Env,
        pos: Pos,
        what: &str,
    ) -> Result<(), ZuError> {
        let got = self.infer(expr, Some(want), env)?;
        if &got != want {
            return Err(ZuError::at(pos, format!("{what}, got {got}")));
        }
        Ok(())
    }

    /// Which record a `{ f = e }` literal builds: the expected type if it's a
    /// record, else the unique record whose field-name set matches.
    fn resolve_record(
        &self,
        fields: &Record,
        expected: Option<&Ty>,
        pos: Pos,
    ) -> Result<String, ZuError> {
        if let Some(Ty::Record(name)) = expected {
            return Ok(name.clone());
        }
        let got: BTreeSet<&str> = fields.iter().map(|(n, _, _)| n.as_str()).collect();
        let mut matches = self.records.iter().filter(|(name, defs)| {
            *name != MODEL
                && defs.len() == got.len()
                && defs.iter().all(|(n, _)| got.contains(n.as_str()))
        });
        match (matches.next(), matches.next()) {
            (Some((name, _)), None) => Ok(name.clone()),
            (None, _) => Err(ZuError::at(
                pos,
                "this record literal matches no `record` declaration",
            )),
            (Some(_), Some(_)) => Err(ZuError::at(
                pos,
                "ambiguous record literal — more than one record has these fields",
            )),
        }
    }

    fn check_record_fields(
        &self,
        name: &str,
        fields: &Record,
        env: &Env,
        pos: Pos,
    ) -> Result<(), ZuError> {
        let defs = self.records[name].clone();
        let mut seen = BTreeSet::new();
        for (fname, fexpr, fpos) in fields {
            let Some((_, want)) = defs.iter().find(|(n, _)| n == fname) else {
                return Err(ZuError::at(*fpos, format!("{name} has no field `{fname}`")));
            };
            if !seen.insert(fname.clone()) {
                return Err(ZuError::at(*fpos, format!("field `{fname}` set twice")));
            }
            self.expect(fexpr, want, env, &format!("field `{fname}`"))?;
        }
        for (fname, _) in &defs {
            if !seen.contains(fname) {
                return Err(ZuError::at(
                    pos,
                    format!("record literal is missing field `{fname}` of {name}"),
                ));
            }
        }
        Ok(())
    }
}

fn check_ty_refs(ty: &Ty, names: &BTreeSet<String>, pos: Pos) -> Result<(), ZuError> {
    match ty {
        Ty::List(t) => check_ty_refs(t, names, pos),
        Ty::Record(n) if !names.contains(n) => Err(ZuError::at(pos, format!("unknown type `{n}`"))),
        _ => Ok(()),
    }
}

fn pos_of(expr: &Expr) -> Pos {
    match expr {
        Expr::Var(_, p)
        | Expr::Field(_, _, p)
        | Expr::Show(_, p)
        | Expr::Len(_, p)
        | Expr::Sum(_, p)
        | Expr::ToInt(_, p)
        | Expr::Nth(_, _, _, p)
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
