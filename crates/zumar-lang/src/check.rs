//! Type checking for zumar-lang v0. Monomorphic and total: every expression
//! gets exactly one of Int/String/Bool, `init` must build the whole model,
//! and every declared message must have exactly one `update` equation —
//! the Elm guarantee that no click can hit a hole.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::*;

pub fn check(app: &App) -> Result<(), ZuError> {
    let mut fields: BTreeMap<&str, Ty> = BTreeMap::new();
    for (name, ty, pos) in &app.model {
        if fields.insert(name, *ty).is_some() {
            return Err(ZuError::at(*pos, format!("duplicate model field `{name}`")));
        }
    }

    let mut msgs: BTreeSet<&str> = BTreeSet::new();
    for (name, pos) in &app.msgs {
        if !msgs.insert(name) {
            return Err(ZuError::at(*pos, format!("duplicate message `{name}`")));
        }
    }

    // init: every field, no strays, right types.
    let mut seen = BTreeSet::new();
    for (name, expr, pos) in &app.init {
        let Some(&want) = fields.get(name.as_str()) else {
            return Err(ZuError::at(
                *pos,
                format!("`init` sets unknown field `{name}`"),
            ));
        };
        if !seen.insert(name.as_str()) {
            return Err(ZuError::at(*pos, format!("`init` sets `{name}` twice")));
        }
        expect_ty(expr, want, &fields, &format!("init field `{name}`"))?;
    }
    for (name, _, pos) in &app.model {
        if !seen.contains(name.as_str()) {
            return Err(ZuError::at(
                *pos,
                format!("`init` is missing field `{name}`"),
            ));
        }
    }

    // updates: known msg, no duplicate equations, known fields, right types.
    let mut equations = BTreeSet::new();
    for (msg, record, pos) in &app.updates {
        if !msgs.contains(msg.as_str()) {
            return Err(ZuError::at(
                *pos,
                format!("`update {msg}` refers to an undeclared message"),
            ));
        }
        if !equations.insert(msg.as_str()) {
            return Err(ZuError::at(
                *pos,
                format!("duplicate `update {msg}` equation"),
            ));
        }
        let mut set = BTreeSet::new();
        for (name, expr, fpos) in record {
            let Some(&want) = fields.get(name.as_str()) else {
                return Err(ZuError::at(
                    *fpos,
                    format!("`update {msg}` sets unknown field `{name}`"),
                ));
            };
            if !set.insert(name.as_str()) {
                return Err(ZuError::at(
                    *fpos,
                    format!("`update {msg}` sets `{name}` twice"),
                ));
            }
            expect_ty(
                expr,
                want,
                &fields,
                &format!("`update {msg}`, field `{name}`"),
            )?;
        }
    }
    // Totality: every message handled.
    for (msg, pos) in &app.msgs {
        if !equations.contains(msg.as_str()) {
            return Err(ZuError::at(
                *pos,
                format!("message `{msg}` has no `update {msg} = ...` equation — every message must be handled"),
            ));
        }
    }

    check_element(&app.view, &fields, &msgs)
}

fn check_element(
    el: &Element,
    fields: &BTreeMap<&str, Ty>,
    msgs: &BTreeSet<&str>,
) -> Result<(), ZuError> {
    for attr in &el.attrs {
        match attr {
            Attr::Str { name, value, .. } => {
                expect_ty(value, Ty::Str, fields, &format!("attribute `{name}`"))?;
            }
            Attr::OnClick { msg, pos } => {
                if !msgs.contains(msg.as_str()) {
                    return Err(ZuError::at(
                        *pos,
                        format!("`onClick {msg}` refers to an undeclared message"),
                    ));
                }
            }
        }
    }
    for child in &el.children {
        match child {
            Child::Elem(e) => check_element(e, fields, msgs)?,
            Child::Text(expr, _) => expect_ty(expr, Ty::Str, fields, "`text`")?,
        }
    }
    Ok(())
}

fn expect_ty(expr: &Expr, want: Ty, fields: &BTreeMap<&str, Ty>, ctx: &str) -> Result<(), ZuError> {
    let got = infer(expr, fields)?;
    if got != want {
        let pos = pos_of(expr);
        return Err(ZuError::at(
            pos,
            format!("{ctx} expects {want}, but this expression is {got}"),
        ));
    }
    Ok(())
}

pub fn infer(expr: &Expr, fields: &BTreeMap<&str, Ty>) -> Result<Ty, ZuError> {
    match expr {
        Expr::Int(_) => Ok(Ty::Int),
        Expr::Str(_) => Ok(Ty::Str),
        Expr::Bool(_) => Ok(Ty::Bool),
        Expr::Field(name, pos) => fields
            .get(name.as_str())
            .copied()
            .ok_or_else(|| ZuError::at(*pos, format!("model has no field `{name}`"))),
        Expr::Show(inner, pos) => {
            let t = infer(inner, fields)?;
            if t != Ty::Int {
                return Err(ZuError::at(*pos, format!("show(..) takes an Int, got {t}")));
            }
            Ok(Ty::Str)
        }
        Expr::Bin(op, l, r, pos) => {
            let lt = infer(l, fields)?;
            let rt = infer(r, fields)?;
            match op {
                Op::Add | Op::Sub | Op::Mul => {
                    if lt != Ty::Int || rt != Ty::Int {
                        return Err(ZuError::at(
                            *pos,
                            format!("arithmetic needs Int on both sides, got {lt} and {rt} (use `++` to join strings)"),
                        ));
                    }
                    Ok(Ty::Int)
                }
                Op::Concat => {
                    if lt != Ty::Str || rt != Ty::Str {
                        return Err(ZuError::at(
                            *pos,
                            format!(
                                "`++` joins Strings, got {lt} and {rt} (use show(..) for numbers)"
                            ),
                        ));
                    }
                    Ok(Ty::Str)
                }
                Op::Eq => {
                    if lt != rt {
                        return Err(ZuError::at(
                            *pos,
                            format!("`==` compares equal types, got {lt} and {rt}"),
                        ));
                    }
                    Ok(Ty::Bool)
                }
                Op::Lt | Op::Gt => {
                    if lt != Ty::Int || rt != Ty::Int {
                        return Err(ZuError::at(
                            *pos,
                            format!("`<`/`>` compare Ints, got {lt} and {rt}"),
                        ));
                    }
                    Ok(Ty::Bool)
                }
            }
        }
        Expr::If(c, t, e, pos) => {
            let ct = infer(c, fields)?;
            if ct != Ty::Bool {
                return Err(ZuError::at(
                    *pos,
                    format!("`if` condition must be Bool, got {ct}"),
                ));
            }
            let tt = infer(t, fields)?;
            let et = infer(e, fields)?;
            if tt != et {
                return Err(ZuError::at(
                    *pos,
                    format!("`if` branches disagree: then is {tt}, else is {et}"),
                ));
            }
            Ok(tt)
        }
    }
}

fn pos_of(expr: &Expr) -> Pos {
    match expr {
        Expr::Field(_, p) | Expr::Show(_, p) | Expr::Bin(_, _, _, p) | Expr::If(_, _, _, p) => *p,
        _ => Pos { line: 0, col: 0 },
    }
}
