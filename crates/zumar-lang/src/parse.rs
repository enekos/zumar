//! Recursive-descent parser for zumar-lang.
//!
//! ```text
//! program := "app" IDENT decl*
//! decl    := "record" IDENT "{" (IDENT ":" ty),* "}"
//!          | "model" "{" (IDENT ":" ty),* "}"
//!          | "init" "=" record cmds?
//!          | "msg" msgitem ("|" msgitem)*
//!          | "update" IDENT IDENT? "=" record cmds?
//!          | "sub" "=" subexpr
//!          | "view" "=" element
//! cmds    := "then" cmdcall ("," cmdcall)*
//! cmdcall := "delay" "(" INT "," IDENT ")" | "httpGet" "(" expr "," IDENT ")"
//! subexpr := "if" expr "then" subexpr "else" subexpr
//!          | "[" ("every" "(" INT "," IDENT ")"),* "]"
//! msgitem := IDENT ty?
//! ty      := "Int" | "String" | "Bool" | "List" ty | "Maybe" ty | IDENT
//! record  := "{" (IDENT "=" expr),* "}"
//! element := IDENT "[" attr,* "]" "[" child,* "]"
//! attr    := "onClick"|"onChange"|"onSubmit" msgcall
//!          | "onInput"|"onCheck" IDENT
//!          | IDENT expr
//! msgcall := IDENT ("(" expr ")")?
//! child   := "text" expr | "for" IDENT "in" expr "{" element "}" | element
//! expr    := "if" expr "then" expr "else" expr
//!          | "for" IDENT "in" expr ("where" expr)? "yield" expr
//!          | "case" expr "of" arm "|" arm     (one none, one some x)
//!          | cmp
//! arm     := "none" "->" expr | "some" IDENT "->" expr
//! cmp     := add (("=="|"!="|"<"|">") add)?
//! add     := mul (("+"|"-"|"++") mul)*
//! mul     := unary (("*"|"/"|"%") unary)*
//! unary   := "not" unary | postfix
//! postfix := atom ("." IDENT)*
//! atom    := INT | STRING | "true" | "false" | "none" | IDENT
//!          | "show"|"length"|"sum"|"toInt"|"reverse"|"head"|"some" "(" expr ")"
//!          | "nth" "(" expr "," expr "," expr ")"
//!          | "[" expr,* "]" | "{" recordbody "}" | "(" expr ")" | "-" atom
//! ```

use crate::ast::*;
use crate::lex::{lex, Tok, Token};

/// Declaration keywords, which therefore can't be used as a bare message
/// payload type — lets `msg Reverse` followed by `update ...` parse without
/// mistaking `update` for Reverse's payload.
const KEYWORDS: &[&str] = &[
    "app", "record", "model", "init", "msg", "update", "sub", "view",
];

pub fn parse(src: &str) -> Result<App, ZuError> {
    let toks = lex(src)?;
    Parser {
        toks,
        i: 0,
        depth: 0,
    }
    .program()
}

/// Recursion guard for `expr`/`element`: pathological nesting must produce a
/// clean error, not a compiler stack overflow. Each guarded level fans out
/// into ~8 precedence frames, so the cap stays well under what a 2 MB thread
/// stack holds — and far above any real program's nesting.
const MAX_DEPTH: usize = 96;

struct Parser {
    toks: Vec<Token>,
    i: usize,
    depth: usize,
}

impl Parser {
    fn descend(&mut self) -> Result<(), ZuError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ZuError::at(
                self.peek().pos,
                format!("nesting deeper than {MAX_DEPTH} levels — flatten the expression or view"),
            ));
        }
        Ok(())
    }

    fn peek(&self) -> &Token {
        &self.toks[self.i]
    }

    fn peek2(&self) -> &Tok {
        &self.toks[(self.i + 1).min(self.toks.len() - 1)].tok
    }

    fn next(&mut self) -> Token {
        let t = self.toks[self.i].clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }

    fn expect(&mut self, tok: Tok) -> Result<Pos, ZuError> {
        let t = self.next();
        if t.tok == tok {
            Ok(t.pos)
        } else {
            Err(ZuError::at(
                t.pos,
                format!("expected {tok}, found {}", t.tok),
            ))
        }
    }

    fn ident(&mut self, what: &str) -> Result<(String, Pos), ZuError> {
        let t = self.next();
        match t.tok {
            Tok::Ident(s) => Ok((s, t.pos)),
            other => Err(ZuError::at(
                t.pos,
                format!("expected {what}, found {other}"),
            )),
        }
    }

    fn eat_ident(&mut self, kw: &str) -> bool {
        if matches!(&self.peek().tok, Tok::Ident(s) if s == kw) {
            self.next();
            true
        } else {
            false
        }
    }

    fn peek_ident_is(&self, kw: &str) -> bool {
        matches!(&self.peek().tok, Tok::Ident(s) if s == kw)
    }

    fn program(&mut self) -> Result<App, ZuError> {
        let start = self.peek().pos;
        if !self.eat_ident("app") {
            return Err(ZuError::at(
                start,
                "a zumar program starts with `app <Name>`",
            ));
        }
        let (name, _) = self.ident("app name")?;

        let mut records = Vec::new();
        let mut model = None;
        let mut init = None;
        let mut init_cmds = Vec::new();
        let mut msgs = None;
        let mut updates = Vec::new();
        let mut subs = None;
        let mut view = None;

        loop {
            let t = self.peek().clone();
            match &t.tok {
                Tok::Eof => break,
                Tok::Ident(kw) => match kw.as_str() {
                    "record" => {
                        self.next();
                        records.push(self.record_decl()?);
                    }
                    "model" => {
                        self.next();
                        if model.replace(self.field_types()?).is_some() {
                            return Err(ZuError::at(t.pos, "duplicate `model` declaration"));
                        }
                    }
                    "init" => {
                        self.next();
                        self.expect(Tok::Eq)?;
                        if init.replace(self.record()?).is_some() {
                            return Err(ZuError::at(t.pos, "duplicate `init` declaration"));
                        }
                        init_cmds = self.cmds()?;
                    }
                    "msg" => {
                        self.next();
                        if msgs.replace(self.msg_decl()?).is_some() {
                            return Err(ZuError::at(t.pos, "duplicate `msg` declaration"));
                        }
                    }
                    "update" => {
                        self.next();
                        let (msg, pos) = self.ident("message name after `update`")?;
                        let var = match &self.peek().tok {
                            Tok::Ident(_) => Some(self.ident("payload variable")?),
                            _ => None,
                        };
                        self.expect(Tok::Eq)?;
                        let fields = self.record()?;
                        let cmds = self.cmds()?;
                        updates.push(Update {
                            msg,
                            var,
                            fields,
                            cmds,
                            pos,
                        });
                    }
                    "sub" => {
                        self.next();
                        self.expect(Tok::Eq)?;
                        if subs.replace(self.sub_expr()?).is_some() {
                            return Err(ZuError::at(t.pos, "duplicate `sub` declaration"));
                        }
                    }
                    "view" => {
                        self.next();
                        self.expect(Tok::Eq)?;
                        if view.replace(self.element()?).is_some() {
                            return Err(ZuError::at(t.pos, "duplicate `view` declaration"));
                        }
                    }
                    other => {
                        let msg = format!(
                            "expected a declaration (record/model/init/msg/update/sub/view), found `{other}`"
                        );
                        return Err(ZuError::at(t.pos, msg));
                    }
                },
                other => {
                    return Err(ZuError::at(
                        t.pos,
                        format!("expected a declaration, found {other}"),
                    ))
                }
            }
        }

        let missing = |what: &str| ZuError::at(start, format!("missing `{what}` declaration"));
        Ok(App {
            name,
            records,
            model: model.ok_or_else(|| missing("model"))?,
            init: init.ok_or_else(|| missing("init"))?,
            init_cmds,
            msgs: msgs.ok_or_else(|| missing("msg"))?,
            updates,
            subs,
            view: view.ok_or_else(|| missing("view"))?,
        })
    }

    fn ty(&mut self) -> Result<Ty, ZuError> {
        if self.eat_ident("List") {
            return Ok(Ty::List(Box::new(self.ty()?)));
        }
        if self.eat_ident("Maybe") {
            return Ok(Ty::Maybe(Box::new(self.ty()?)));
        }
        let (name, _) = self.ident("a type")?;
        Ok(match name.as_str() {
            "Int" => Ty::Int,
            "String" => Ty::Str,
            "Bool" => Ty::Bool,
            _ => Ty::Record(name),
        })
    }

    fn field_types(&mut self) -> Result<Vec<(String, Ty, Pos)>, ZuError> {
        self.expect(Tok::LBrace)?;
        let mut fields = Vec::new();
        if self.peek().tok != Tok::RBrace {
            loop {
                let (name, pos) = self.ident("field name")?;
                self.expect(Tok::Colon)?;
                fields.push((name, self.ty()?, pos));
                if self.peek().tok != Tok::Comma {
                    break;
                }
                self.next();
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(fields)
    }

    fn record_decl(&mut self) -> Result<RecordDef, ZuError> {
        let (name, pos) = self.ident("record name")?;
        let fields = self.field_types()?;
        Ok(RecordDef { name, fields, pos })
    }

    fn msg_decl(&mut self) -> Result<Vec<MsgDef>, ZuError> {
        let mut msgs = vec![self.msg_item()?];
        while self.peek().tok == Tok::Pipe {
            self.next();
            msgs.push(self.msg_item()?);
        }
        Ok(msgs)
    }

    fn msg_item(&mut self) -> Result<MsgDef, ZuError> {
        let (name, pos) = self.ident("message name")?;
        // A payload type follows only if the next token is a type ident that
        // isn't a declaration keyword (so `msg Reverse` before `update` is
        // payload-free).
        let payload = match &self.peek().tok {
            Tok::Ident(s) if !KEYWORDS.contains(&s.as_str()) => Some(self.ty()?),
            _ => None,
        };
        Ok(MsgDef { name, payload, pos })
    }

    fn record(&mut self) -> Result<Record, ZuError> {
        self.expect(Tok::LBrace)?;
        let fields = self.record_fields()?;
        self.expect(Tok::RBrace)?;
        Ok(fields)
    }

    /// `field = expr (, field = expr)*` — the shared body of record literals,
    /// updates, and init/update RHS. Caller owns the braces.
    fn record_fields(&mut self) -> Result<Record, ZuError> {
        let mut fields = Vec::new();
        if self.peek().tok != Tok::RBrace {
            loop {
                let (name, pos) = self.ident("field name")?;
                self.expect(Tok::Eq)?;
                fields.push((name, self.expr()?, pos));
                if self.peek().tok != Tok::Comma {
                    break;
                }
                self.next();
            }
        }
        Ok(fields)
    }

    /// `then cmd (, cmd)*` after an init/update record — or nothing.
    fn cmds(&mut self) -> Result<Vec<CmdCall>, ZuError> {
        if !self.eat_ident("then") {
            return Ok(Vec::new());
        }
        let mut cmds = vec![self.cmd_call()?];
        while self.peek().tok == Tok::Comma {
            self.next();
            cmds.push(self.cmd_call()?);
        }
        Ok(cmds)
    }

    fn cmd_call(&mut self) -> Result<CmdCall, ZuError> {
        let (name, pos) = self.ident("a command (delay, httpGet)")?;
        match name.as_str() {
            "delay" => {
                self.expect(Tok::LParen)?;
                let ms = self.int_lit("delay milliseconds")?;
                self.expect(Tok::Comma)?;
                let (msg, _) = self.ident("message name")?;
                self.expect(Tok::RParen)?;
                Ok(CmdCall::Delay { ms, msg, pos })
            }
            "httpGet" => {
                self.expect(Tok::LParen)?;
                let url = self.expr()?;
                self.expect(Tok::Comma)?;
                let (ctor, _) = self.ident("message constructor")?;
                self.expect(Tok::RParen)?;
                Ok(CmdCall::HttpGet { url, ctor, pos })
            }
            other => Err(ZuError::at(
                pos,
                format!("unknown command `{other}` (available: delay, httpGet)"),
            )),
        }
    }

    /// `sub =` body: a bracket list of `every(...)`, or an if choosing
    /// between two sub expressions.
    fn sub_expr(&mut self) -> Result<SubExpr, ZuError> {
        if self.peek_ident_is("if") {
            let pos = self.next().pos;
            let cond = self.expr()?;
            if !self.eat_ident("then") {
                return Err(ZuError::at(self.peek().pos, "expected `then`"));
            }
            let t = self.sub_expr()?;
            if !self.eat_ident("else") {
                return Err(ZuError::at(self.peek().pos, "expected `else`"));
            }
            let f = self.sub_expr()?;
            return Ok(SubExpr::If(cond, Box::new(t), Box::new(f), pos));
        }
        self.expect(Tok::LBracket)?;
        let mut calls = Vec::new();
        if self.peek().tok != Tok::RBracket {
            loop {
                let (name, pos) = self.ident("a subscription (every)")?;
                if name != "every" {
                    return Err(ZuError::at(
                        pos,
                        format!("unknown subscription `{name}` (available: every)"),
                    ));
                }
                self.expect(Tok::LParen)?;
                let ms = self.int_lit("interval milliseconds")?;
                self.expect(Tok::Comma)?;
                let (msg, _) = self.ident("message name")?;
                self.expect(Tok::RParen)?;
                calls.push(SubCall { ms, msg, pos });
                if self.peek().tok != Tok::Comma {
                    break;
                }
                self.next();
            }
        }
        self.expect(Tok::RBracket)?;
        Ok(SubExpr::List(calls))
    }

    fn int_lit(&mut self, what: &str) -> Result<i64, ZuError> {
        let t = self.next();
        match t.tok {
            Tok::Int(n) => Ok(n),
            other => Err(ZuError::at(
                t.pos,
                format!("expected {what} as an integer literal, found {other}"),
            )),
        }
    }

    fn element(&mut self) -> Result<Element, ZuError> {
        self.descend()?;
        let result = self.element_inner();
        self.depth -= 1;
        result
    }

    fn element_inner(&mut self) -> Result<Element, ZuError> {
        let (tag, _) = self.ident("element tag")?;
        self.expect(Tok::LBracket)?;
        let mut attrs = Vec::new();
        if self.peek().tok != Tok::RBracket {
            loop {
                attrs.push(self.attr()?);
                if self.peek().tok != Tok::Comma {
                    break;
                }
                self.next();
            }
        }
        self.expect(Tok::RBracket)?;

        self.expect(Tok::LBracket)?;
        let mut children = Vec::new();
        if self.peek().tok != Tok::RBracket {
            loop {
                children.push(self.child()?);
                if self.peek().tok != Tok::Comma {
                    break;
                }
                self.next();
            }
        }
        self.expect(Tok::RBracket)?;
        Ok(Element {
            tag,
            attrs,
            children,
        })
    }

    fn attr(&mut self) -> Result<Attr, ZuError> {
        let (name, pos) = self.ident("attribute")?;
        match name.as_str() {
            "onClick" => Ok(Attr::On {
                event: "click".into(),
                handler: self.msg_call()?,
                prevent_default: false,
            }),
            "onChange" => Ok(Attr::On {
                event: "change".into(),
                handler: self.msg_call()?,
                prevent_default: false,
            }),
            "onSubmit" => Ok(Attr::On {
                event: "submit".into(),
                handler: self.msg_call()?,
                prevent_default: true,
            }),
            "onInput" => {
                let (ctor, cpos) = self.ident("message constructor after `onInput`")?;
                Ok(Attr::OnValue {
                    event: "input".into(),
                    ctor,
                    kind: ValueKind::Value,
                    pos: cpos,
                })
            }
            "onCheck" => {
                let (ctor, cpos) = self.ident("message constructor after `onCheck`")?;
                Ok(Attr::OnValue {
                    event: "change".into(),
                    ctor,
                    kind: ValueKind::Checked,
                    pos: cpos,
                })
            }
            _ => Ok(Attr::Str {
                name,
                value: self.expr()?,
                pos,
            }),
        }
    }

    fn msg_call(&mut self) -> Result<MsgCall, ZuError> {
        let (name, pos) = self.ident("a message")?;
        let arg = if self.peek().tok == Tok::LParen {
            self.next();
            let e = self.expr()?;
            self.expect(Tok::RParen)?;
            Some(e)
        } else {
            None
        };
        Ok(MsgCall { name, arg, pos })
    }

    fn child(&mut self) -> Result<Child, ZuError> {
        if self.peek_ident_is("text") {
            let pos = self.next().pos;
            return Ok(Child::Text(self.expr()?, pos));
        }
        if self.peek_ident_is("for") {
            let pos = self.next().pos;
            let (var, _) = self.ident("loop variable after `for`")?;
            if !self.eat_ident("in") {
                return Err(ZuError::at(self.peek().pos, "expected `in`"));
            }
            let list = self.expr()?;
            self.expect(Tok::LBrace)?;
            let body = self.element()?;
            self.expect(Tok::RBrace)?;
            return Ok(Child::For {
                var,
                list,
                body: Box::new(body),
                pos,
            });
        }
        Ok(Child::Elem(self.element()?))
    }

    fn expr(&mut self) -> Result<Expr, ZuError> {
        self.descend()?;
        let result = self.expr_inner();
        self.depth -= 1;
        result
    }

    fn expr_inner(&mut self) -> Result<Expr, ZuError> {
        if self.peek_ident_is("if") {
            let pos = self.next().pos;
            let cond = self.expr()?;
            if !self.eat_ident("then") {
                return Err(ZuError::at(self.peek().pos, "expected `then`"));
            }
            let then = self.expr()?;
            if !self.eat_ident("else") {
                return Err(ZuError::at(
                    self.peek().pos,
                    "expected `else` (zumar `if` is an expression; both branches are required)",
                ));
            }
            let els = self.expr()?;
            return Ok(Expr::If(Box::new(cond), Box::new(then), Box::new(els), pos));
        }
        if self.peek_ident_is("for") {
            let pos = self.next().pos;
            let (var, _) = self.ident("loop variable after `for`")?;
            if !self.eat_ident("in") {
                return Err(ZuError::at(self.peek().pos, "expected `in`"));
            }
            let list = self.expr()?;
            let cond = if self.eat_ident("where") {
                Some(Box::new(self.expr()?))
            } else {
                None
            };
            if !self.eat_ident("yield") {
                return Err(ZuError::at(
                    self.peek().pos,
                    "expected `yield` (a comprehension is `for x in xs [where c] yield e`)",
                ));
            }
            let body = self.expr()?;
            return Ok(Expr::For {
                var,
                list: Box::new(list),
                cond,
                body: Box::new(body),
                pos,
            });
        }
        if self.peek_ident_is("case") {
            return self.case_expr();
        }
        self.cmp()
    }

    /// `case <scrut> of none -> <e> | some <x> -> <e>` — arms in either
    /// order, exactly one `none` and one `some`.
    fn case_expr(&mut self) -> Result<Expr, ZuError> {
        let pos = self.next().pos; // `case`
        let scrut = self.expr()?;
        if !self.eat_ident("of") {
            return Err(ZuError::at(
                self.peek().pos,
                "expected `of` after the `case` scrutinee",
            ));
        }
        let mut none_arm = None;
        let mut some = None;
        for i in 0..2 {
            if i == 1 {
                self.expect(Tok::Pipe)?;
            }
            if self.eat_ident("none") {
                self.expect(Tok::Arrow)?;
                if none_arm.replace(self.expr()?).is_some() {
                    return Err(ZuError::at(pos, "duplicate `none` arm"));
                }
            } else if self.eat_ident("some") {
                let (var, _) = self.ident("variable after `some`")?;
                self.expect(Tok::Arrow)?;
                if some.replace((var, self.expr()?)).is_some() {
                    return Err(ZuError::at(pos, "duplicate `some` arm"));
                }
            } else {
                return Err(ZuError::at(self.peek().pos, "expected `none` or `some x`"));
            }
        }
        let none_arm =
            none_arm.ok_or_else(|| ZuError::at(pos, "`case` is missing the `none` arm"))?;
        let (some_var, some_arm) =
            some.ok_or_else(|| ZuError::at(pos, "`case` is missing the `some` arm"))?;
        Ok(Expr::Case {
            scrut: Box::new(scrut),
            none_arm: Box::new(none_arm),
            some_var,
            some_arm: Box::new(some_arm),
            pos,
        })
    }

    fn cmp(&mut self) -> Result<Expr, ZuError> {
        let lhs = self.add()?;
        let op = match self.peek().tok {
            Tok::EqEq => Op::Eq,
            Tok::Ne => Op::Ne,
            Tok::Lt => Op::Lt,
            Tok::Gt => Op::Gt,
            _ => return Ok(lhs),
        };
        let pos = self.next().pos;
        let rhs = self.add()?;
        Ok(Expr::Bin(op, Box::new(lhs), Box::new(rhs), pos))
    }

    fn add(&mut self) -> Result<Expr, ZuError> {
        let mut lhs = self.mul()?;
        loop {
            let op = match self.peek().tok {
                Tok::Plus => Op::Add,
                Tok::Minus => Op::Sub,
                Tok::PlusPlus => Op::Concat,
                _ => return Ok(lhs),
            };
            let pos = self.next().pos;
            let rhs = self.mul()?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs), pos);
        }
    }

    fn mul(&mut self) -> Result<Expr, ZuError> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek().tok {
                Tok::Star => Op::Mul,
                Tok::Slash => Op::Div,
                Tok::Percent => Op::Rem,
                _ => return Ok(lhs),
            };
            let pos = self.next().pos;
            let rhs = self.unary()?;
            lhs = Expr::Bin(op, Box::new(lhs), Box::new(rhs), pos);
        }
    }

    fn unary(&mut self) -> Result<Expr, ZuError> {
        if self.peek_ident_is("not") {
            let pos = self.next().pos;
            return Ok(Expr::Not(Box::new(self.unary()?), pos));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Expr, ZuError> {
        let mut e = self.atom()?;
        while self.peek().tok == Tok::Dot {
            let pos = self.next().pos;
            let (field, _) = self.ident("field name after `.`")?;
            e = Expr::Field(Box::new(e), field, pos);
        }
        Ok(e)
    }

    fn atom(&mut self) -> Result<Expr, ZuError> {
        let t = self.next();
        match t.tok {
            Tok::Int(n) => Ok(Expr::Int(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Minus => Ok(Expr::Bin(
                Op::Sub,
                Box::new(Expr::Int(0)),
                Box::new(self.atom()?),
                t.pos,
            )),
            Tok::LParen => {
                let e = self.expr()?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Tok::LBracket => {
                let mut items = Vec::new();
                if self.peek().tok != Tok::RBracket {
                    loop {
                        items.push(self.expr()?);
                        if self.peek().tok != Tok::Comma {
                            break;
                        }
                        self.next();
                    }
                }
                self.expect(Tok::RBracket)?;
                Ok(Expr::ListLit(items, t.pos))
            }
            Tok::LBrace => self.record_expr(t.pos),
            Tok::Ident(s) => match s.as_str() {
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                "show" => Ok(Expr::Show(Box::new(self.paren_arg()?), t.pos)),
                "length" => Ok(Expr::Len(Box::new(self.paren_arg()?), t.pos)),
                "sum" => Ok(Expr::Sum(Box::new(self.paren_arg()?), t.pos)),
                "toInt" => Ok(Expr::ToInt(Box::new(self.paren_arg()?), t.pos)),
                "reverse" => Ok(Expr::Reverse(Box::new(self.paren_arg()?), t.pos)),
                "head" => Ok(Expr::Head(Box::new(self.paren_arg()?), t.pos)),
                "none" => Ok(Expr::None(t.pos)),
                "some" => Ok(Expr::Some(Box::new(self.paren_arg()?), t.pos)),
                "nth" => {
                    let args = self.paren_args()?;
                    let [list, index, default] = <[Expr; 3]>::try_from(args).map_err(|v| {
                        ZuError::at(
                            t.pos,
                            format!(
                                "nth(list, index, default) takes 3 arguments, got {}",
                                v.len()
                            ),
                        )
                    })?;
                    Ok(Expr::Nth(
                        Box::new(list),
                        Box::new(index),
                        Box::new(default),
                        t.pos,
                    ))
                }
                _ => Ok(Expr::Var(s, t.pos)),
            },
            other => Err(ZuError::at(
                t.pos,
                format!("expected an expression, found {other}"),
            )),
        }
    }

    fn paren_arg(&mut self) -> Result<Expr, ZuError> {
        self.expect(Tok::LParen)?;
        let e = self.expr()?;
        self.expect(Tok::RParen)?;
        Ok(e)
    }

    fn paren_args(&mut self) -> Result<Vec<Expr>, ZuError> {
        self.expect(Tok::LParen)?;
        let mut args = Vec::new();
        if self.peek().tok != Tok::RParen {
            loop {
                args.push(self.expr()?);
                if self.peek().tok != Tok::Comma {
                    break;
                }
                self.next();
            }
        }
        self.expect(Tok::RParen)?;
        Ok(args)
    }

    /// A `{...}` in expression position: record literal `{ f = e }` or record
    /// update `{ base | f = e }`, disambiguated by the token after the first
    /// name (`=` -> literal, otherwise -> update). The opening brace is
    /// already consumed.
    fn record_expr(&mut self, pos: Pos) -> Result<Expr, ZuError> {
        if self.peek().tok == Tok::RBrace {
            self.next();
            return Ok(Expr::RecordLit(Vec::new(), pos));
        }
        let is_literal = matches!(self.peek().tok, Tok::Ident(_)) && *self.peek2() == Tok::Eq;
        if is_literal {
            let fields = self.record_fields()?;
            self.expect(Tok::RBrace)?;
            return Ok(Expr::RecordLit(fields, pos));
        }
        let base = self.expr()?;
        self.expect(Tok::Pipe)?;
        let fields = self.record_fields()?;
        self.expect(Tok::RBrace)?;
        Ok(Expr::RecordUpdate(Box::new(base), fields, pos))
    }
}
