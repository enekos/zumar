//! Recursive-descent parser for zumar-lang v0.
//!
//! ```text
//! program  := "app" IDENT decl*
//! decl     := "model" "{" (IDENT ":" ty),* "}"
//!           | "init" "=" record
//!           | "msg" IDENT ("|" IDENT)*
//!           | "update" IDENT "=" record
//!           | "view" "=" element
//! record   := "{" (IDENT "=" expr),* "}"
//! element  := IDENT "[" (attr),* "]" "[" (child),* "]"
//! attr     := "onClick" IDENT | IDENT expr
//! child    := "text" expr | element
//! expr     := "if" expr "then" expr "else" expr | cmp
//! cmp      := add (("==" | "<" | ">") add)?
//! add      := mul (("+" | "-" | "++") mul)*
//! mul      := atom ("*" atom)*
//! atom     := INT | STRING | "true" | "false" | "model" "." IDENT
//!           | "show" "(" expr ")" | "(" expr ")" | "-" atom
//! ```

use crate::ast::*;
use crate::lex::{lex, Tok, Token};

pub fn parse(src: &str) -> Result<App, ZuError> {
    let toks = lex(src)?;
    Parser { toks, i: 0, depth: 0 }.program()
}

/// Recursion guard for `expr`/`element`: pathological nesting must produce
/// a clean error, not a compiler stack overflow.
const MAX_DEPTH: usize = 200;

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
            Err(ZuError::at(t.pos, format!("expected {tok}, found {}", t.tok)))
        }
    }

    fn ident(&mut self, what: &str) -> Result<(String, Pos), ZuError> {
        let t = self.next();
        match t.tok {
            Tok::Ident(s) => Ok((s, t.pos)),
            other => Err(ZuError::at(t.pos, format!("expected {what}, found {other}"))),
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

    fn program(&mut self) -> Result<App, ZuError> {
        let start = self.peek().pos;
        if !self.eat_ident("app") {
            return Err(ZuError::at(start, "a zumar program starts with `app <Name>`"));
        }
        let (name, _) = self.ident("app name")?;

        let mut model = None;
        let mut init = None;
        let mut msgs = None;
        let mut updates = Vec::new();
        let mut view = None;

        loop {
            let t = self.peek().clone();
            match &t.tok {
                Tok::Eof => break,
                Tok::Ident(kw) => match kw.as_str() {
                    "model" => {
                        self.next();
                        if model.replace(self.model_decl()?).is_some() {
                            return Err(ZuError::at(t.pos, "duplicate `model` declaration"));
                        }
                    }
                    "init" => {
                        self.next();
                        self.expect(Tok::Eq)?;
                        if init.replace(self.record()?).is_some() {
                            return Err(ZuError::at(t.pos, "duplicate `init` declaration"));
                        }
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
                        self.expect(Tok::Eq)?;
                        updates.push((msg, self.record()?, pos));
                    }
                    "view" => {
                        self.next();
                        self.expect(Tok::Eq)?;
                        if view.replace(self.element()?).is_some() {
                            return Err(ZuError::at(t.pos, "duplicate `view` declaration"));
                        }
                    }
                    other => {
                        return Err(ZuError::at(
                            t.pos,
                            format!("expected a declaration (model/init/msg/update/view), found `{other}`"),
                        ))
                    }
                },
                other => {
                    return Err(ZuError::at(t.pos, format!("expected a declaration, found {other}")))
                }
            }
        }

        let missing = |what: &str| ZuError::at(start, format!("missing `{what}` declaration"));
        Ok(App {
            name,
            model: model.ok_or_else(|| missing("model"))?,
            init: init.ok_or_else(|| missing("init"))?,
            msgs: msgs.ok_or_else(|| missing("msg"))?,
            updates,
            view: view.ok_or_else(|| missing("view"))?,
        })
    }

    fn model_decl(&mut self) -> Result<Vec<(String, Ty, Pos)>, ZuError> {
        self.expect(Tok::LBrace)?;
        let mut fields = Vec::new();
        if self.peek().tok != Tok::RBrace {
            loop {
                let (name, pos) = self.ident("field name")?;
                self.expect(Tok::Colon)?;
                let (ty_name, ty_pos) = self.ident("type (Int, String, Bool)")?;
                let ty = match ty_name.as_str() {
                    "Int" => Ty::Int,
                    "String" => Ty::Str,
                    "Bool" => Ty::Bool,
                    other => {
                        return Err(ZuError::at(
                            ty_pos,
                            format!("unknown type `{other}` (expected Int, String, or Bool)"),
                        ))
                    }
                };
                fields.push((name, ty, pos));
                if self.peek().tok != Tok::Comma {
                    break;
                }
                self.next();
            }
        }
        self.expect(Tok::RBrace)?;
        Ok(fields)
    }

    fn msg_decl(&mut self) -> Result<Vec<(String, Pos)>, ZuError> {
        let mut msgs = vec![self.ident("message name")?];
        while self.peek().tok == Tok::Pipe {
            self.next();
            msgs.push(self.ident("message name")?);
        }
        Ok(msgs)
    }

    fn record(&mut self) -> Result<Record, ZuError> {
        self.expect(Tok::LBrace)?;
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
        self.expect(Tok::RBrace)?;
        Ok(fields)
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
                let (name, pos) = self.ident("attribute")?;
                if name == "onClick" {
                    let (msg, _) = self.ident("message name after `onClick`")?;
                    attrs.push(Attr::OnClick { msg, pos });
                } else {
                    attrs.push(Attr::Str { name, value: self.expr()?, pos });
                }
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
                if matches!(&self.peek().tok, Tok::Ident(s) if s == "text") {
                    let pos = self.next().pos;
                    children.push(Child::Text(self.expr()?, pos));
                } else {
                    children.push(Child::Elem(self.element()?));
                }
                if self.peek().tok != Tok::Comma {
                    break;
                }
                self.next();
            }
        }
        self.expect(Tok::RBracket)?;
        Ok(Element { tag, attrs, children })
    }

    fn expr(&mut self) -> Result<Expr, ZuError> {
        self.descend()?;
        let result = self.expr_inner();
        self.depth -= 1;
        result
    }

    fn expr_inner(&mut self) -> Result<Expr, ZuError> {
        if matches!(&self.peek().tok, Tok::Ident(s) if s == "if") {
            let pos = self.next().pos;
            let cond = self.expr()?;
            if !self.eat_ident("then") {
                return Err(ZuError::at(self.peek().pos, "expected `then`"));
            }
            let then = self.expr()?;
            if !self.eat_ident("else") {
                return Err(ZuError::at(self.peek().pos, "expected `else` (zumar `if` is an expression; both branches are required)"));
            }
            let els = self.expr()?;
            return Ok(Expr::If(Box::new(cond), Box::new(then), Box::new(els), pos));
        }
        self.cmp()
    }

    fn cmp(&mut self) -> Result<Expr, ZuError> {
        let lhs = self.add()?;
        let op = match self.peek().tok {
            Tok::EqEq => Op::Eq,
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
        let mut lhs = self.atom()?;
        while self.peek().tok == Tok::Star {
            let pos = self.next().pos;
            let rhs = self.atom()?;
            lhs = Expr::Bin(Op::Mul, Box::new(lhs), Box::new(rhs), pos);
        }
        Ok(lhs)
    }

    fn atom(&mut self) -> Result<Expr, ZuError> {
        let t = self.next();
        match t.tok {
            Tok::Int(n) => Ok(Expr::Int(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Minus => {
                let inner = self.atom()?;
                Ok(Expr::Bin(Op::Sub, Box::new(Expr::Int(0)), Box::new(inner), t.pos))
            }
            Tok::LParen => {
                let e = self.expr()?;
                self.expect(Tok::RParen)?;
                Ok(e)
            }
            Tok::Ident(s) => match s.as_str() {
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                "model" => {
                    self.expect(Tok::Dot)?;
                    let (field, pos) = self.ident("field name after `model.`")?;
                    Ok(Expr::Field(field, pos))
                }
                "show" => {
                    self.expect(Tok::LParen)?;
                    let e = self.expr()?;
                    self.expect(Tok::RParen)?;
                    Ok(Expr::Show(Box::new(e), t.pos))
                }
                other => Err(ZuError::at(
                    t.pos,
                    format!("unknown name `{other}` (values come from `model.<field>`; did you mean a string literal?)"),
                )),
            },
            other => Err(ZuError::at(t.pos, format!("expected an expression, found {other}"))),
        }
    }
}
