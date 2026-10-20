//! Moves the `yield` expressions inside other expressions of a generator body to statements
//! of their own, in the order Python evaluates them, so that a generator can suspend there.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_python_ast::{self as ast, AtomicNodeIndex, Expr, ExprContext, Stmt, name::Name};
use ruff_text_size::{Ranged, TextRange};

use crate::error::Error;

/// The statements that do what `statement` does, each `yield` alone as `name = yield value`;
/// the parts that Python evaluates before a `yield` go to temporary names first.
pub(super) fn statement(statement: &Stmt) -> Result<Vec<Stmt>, Error> {
    let mut hoist = Hoist {
        range: statement.range(),
        count: 0,
    };
    let mut out = Vec::new();
    let mut rewritten = statement.clone();
    match &mut rewritten {
        Stmt::Expr(each) => hoist.replace(&mut each.value, &mut out)?,
        Stmt::Assign(each) if !each.targets.iter().any(yields) => {
            hoist.replace(&mut each.value, &mut out)?
        }
        Stmt::AugAssign(each) if !yields(&each.target) => {
            hoist.replace(&mut each.value, &mut out)?
        }
        Stmt::AnnAssign(ast::StmtAnnAssign {
            value: Some(value),
            target,
            ..
        }) if !yields(target) => hoist.replace(value, &mut out)?,
        Stmt::Return(ast::StmtReturn {
            value: Some(value), ..
        }) => hoist.replace(value, &mut out)?,
        Stmt::If(each) if !(each.elif_else_clauses.iter()).any(clause_yields) => {
            hoist.replace(&mut each.test, &mut out)?
        }
        Stmt::For(each) if !yields(&each.target) => hoist.replace(&mut each.iter, &mut out)?,
        Stmt::Match(each)
            if !(each.cases.iter()).any(|case| case.guard.iter().any(|g| yields(g))) =>
        {
            hoist.replace(&mut each.subject, &mut out)?
        }
        Stmt::Raise(each) => {
            let mut parts: Vec<&mut Expr> = (each.exc.iter_mut().chain(each.cause.iter_mut()))
                .map(|part| &mut **part)
                .collect();
            hoist.children(&mut parts, &mut out)?
        }
        _ => return Err(unsupported()),
    }
    out.push(rewritten);
    Ok(out)
}

/// Whether a statement yields outside its bodies, where the generator cannot suspend
/// without moving the `yield` first.
pub(super) fn needed(statement: &Stmt) -> bool {
    let inner = |value: &Expr| match value {
        Expr::Yield(each) => each.value.as_deref().is_some_and(yields),
        Expr::YieldFrom(each) => yields(&each.value),
        other => yields(other),
    };
    match statement {
        Stmt::Expr(each) => inner(&each.value),
        Stmt::Assign(each) => each.targets.iter().any(yields) || inner(&each.value),
        Stmt::If(each) => yields(&each.test) || each.elif_else_clauses.iter().any(clause_yields),
        Stmt::For(each) => yields(&each.target) || yields(&each.iter),
        Stmt::While(each) => yields(&each.test),
        Stmt::With(each) => (each.items.iter()).any(|item| {
            yields(&item.context_expr) || item.optional_vars.as_deref().is_some_and(yields)
        }),
        Stmt::Match(each) => {
            yields(&each.subject)
                || (each.cases.iter()).any(|case| case.guard.as_deref().is_some_and(yields))
        }
        Stmt::Try(_) | Stmt::FunctionDef(_) | Stmt::ClassDef(_) => false,
        other => super::statement::contains_yield(std::slice::from_ref(other)),
    }
}

fn unsupported() -> Error {
    Error::unsupported("generator suspension in this kind of expression")
}

/// Whether an expression yields, not counting the bodies of lambdas.
fn yields(expression: &Expr) -> bool {
    let statement = Stmt::Expr(ast::StmtExpr {
        node_index: AtomicNodeIndex::default(),
        range: expression.range(),
        value: Box::new(expression.clone()),
    });
    super::statement::contains_yield(std::slice::from_ref(&statement))
}

fn clause_yields(clause: &ast::ElifElseClause) -> bool {
    clause.test.as_ref().is_some_and(yields)
}

struct Hoist {
    range: TextRange,
    count: usize,
}

impl Hoist {
    fn temporary(&mut self) -> String {
        self.count += 1;
        format!("__yield_{}", self.count)
    }

    fn name(&self, id: &str, ctx: ExprContext) -> Expr {
        Expr::Name(ast::ExprName {
            node_index: AtomicNodeIndex::default(),
            range: self.range,
            id: Name::new(id),
            ctx,
        })
    }

    fn assign(&self, id: &str, value: Expr) -> Stmt {
        Stmt::Assign(ast::StmtAssign {
            node_index: AtomicNodeIndex::default(),
            range: self.range,
            targets: vec![self.name(id, ExprContext::Store)],
            value: Box::new(value),
        })
    }

    /// Evaluates `value` into a new temporary name and gives the name.
    fn hold(&mut self, value: Expr, out: &mut Vec<Stmt>) -> Expr {
        let id = self.temporary();
        out.push(self.assign(&id, value));
        self.name(&id, ExprContext::Load)
    }

    /// Replaces an expression that yields with one that does not, after `out`.
    fn replace(&mut self, expression: &mut Expr, out: &mut Vec<Stmt>) -> Result<(), Error> {
        if yields(expression) {
            *expression = self.expression(expression, out)?;
        }
        Ok(())
    }

    /// The parts of an expression in the order Python evaluates them: those before the last
    /// that yields are held in temporary names, that one is rewritten, the rest stay.
    fn children(&mut self, parts: &mut [&mut Expr], out: &mut Vec<Stmt>) -> Result<(), Error> {
        let Some(last) = parts.iter().rposition(|part| yields(part)) else {
            return Ok(());
        };
        for part in parts[..last].iter_mut() {
            if yields(part) {
                **part = self.expression(part, out)?;
            }
            if !part.is_literal_expr() {
                **part = self.hold(part.clone(), out);
            }
        }
        self.replace(parts[last], out)
    }

    fn expression(&mut self, expression: &Expr, out: &mut Vec<Stmt>) -> Result<Expr, Error> {
        let mut rewritten = expression.clone();
        match &mut rewritten {
            Expr::Yield(each) => {
                if let Some(value) = &mut each.value {
                    self.replace(value, out)?;
                }
                return Ok(self.hold(rewritten, out));
            }
            Expr::BoolOp(each) => return self.either(each, out),
            Expr::If(each) => return self.choice(each, out),
            Expr::Starred(each) => self.replace(&mut each.value, out)?,
            Expr::Named(each) => self.replace(&mut each.value, out)?,
            Expr::UnaryOp(each) => self.replace(&mut each.operand, out)?,
            Expr::Attribute(each) => self.replace(&mut each.value, out)?,
            Expr::BinOp(each) => self.children(&mut [&mut each.left, &mut each.right], out)?,
            Expr::Subscript(each) => self.children(&mut [&mut each.value, &mut each.slice], out)?,
            Expr::Compare(each) => {
                // A chain evaluates its later operands only while the comparisons hold.
                if each.operands.iter().skip(2).any(yields) {
                    return Err(unsupported());
                }
                let mut parts: Vec<&mut Expr> = each.operands.iter_mut().take(2).collect();
                self.children(&mut parts, out)?
            }
            Expr::Slice(each) => {
                let mut parts: Vec<&mut Expr> = [&mut each.lower, &mut each.upper, &mut each.step]
                    .into_iter()
                    .flatten()
                    .map(|part| &mut **part)
                    .collect();
                self.children(&mut parts, out)?
            }
            Expr::Tuple(ast::ExprTuple { elts, .. })
            | Expr::List(ast::ExprList { elts, .. })
            | Expr::Set(ast::ExprSet { elts, .. }) => {
                let mut parts: Vec<&mut Expr> = elts.iter_mut().map(unstarred).collect();
                self.children(&mut parts, out)?
            }
            Expr::Dict(each) => {
                let mut parts: Vec<&mut Expr> = Vec::new();
                for item in &mut each.items {
                    parts.extend(item.key.as_mut());
                    parts.push(&mut item.value);
                }
                self.children(&mut parts, out)?
            }
            Expr::Call(each) => {
                let mut parts: Vec<&mut Expr> = vec![&mut each.func];
                parts.extend(each.arguments.args.iter_mut().map(unstarred));
                parts.extend(
                    each.arguments
                        .keywords
                        .iter_mut()
                        .map(|word| &mut word.value),
                );
                self.children(&mut parts, out)?
            }
            _ => return Err(unsupported()),
        }
        Ok(rewritten)
    }

    /// `a and b` or `a or b` whose later operands yield: each runs only while the result is
    /// still open, as `if held: held = b`.
    fn either(&mut self, each: &ast::ExprBoolOp, out: &mut Vec<Stmt>) -> Result<Expr, Error> {
        let last = (each.values.iter()).rposition(yields).unwrap_or_default();
        let mut first = each.values[0].clone();
        self.replace(&mut first, out)?;
        let id = self.temporary();
        out.push(self.assign(&id, first));
        for value in &each.values[1..=last] {
            let mut body = Vec::new();
            let mut value = value.clone();
            self.replace(&mut value, &mut body)?;
            body.push(self.assign(&id, value));
            let held = self.name(&id, ExprContext::Load);
            let test = match each.op {
                ast::BoolOp::And => held,
                ast::BoolOp::Or => Expr::UnaryOp(ast::ExprUnaryOp {
                    node_index: AtomicNodeIndex::default(),
                    range: self.range,
                    op: ast::UnaryOp::Not,
                    operand: Box::new(held),
                }),
            };
            out.push(self.branch(test, body, None));
        }
        let held = self.name(&id, ExprContext::Load);
        let rest = &each.values[last + 1..];
        if rest.is_empty() {
            return Ok(held);
        }
        let mut values = vec![held];
        values.extend(rest.iter().cloned());
        Ok(Expr::BoolOp(ast::ExprBoolOp {
            values,
            ..each.clone()
        }))
    }

    /// `a if test else b` whose branches yield: the branch that runs sets a temporary name.
    fn choice(&mut self, each: &ast::ExprIf, out: &mut Vec<Stmt>) -> Result<Expr, Error> {
        let mut test = (*each.test).clone();
        self.replace(&mut test, out)?;
        let id = self.temporary();
        let mut arms = Vec::new();
        for arm in [&each.body, &each.orelse] {
            let mut body = Vec::new();
            let mut value = (**arm).clone();
            self.replace(&mut value, &mut body)?;
            body.push(self.assign(&id, value));
            arms.push(body);
        }
        let orelse = arms.pop();
        let body = arms.pop().unwrap_or_default();
        out.push(self.branch(test, body, orelse));
        Ok(self.name(&id, ExprContext::Load))
    }

    fn branch(&self, test: Expr, body: Vec<Stmt>, orelse: Option<Vec<Stmt>>) -> Stmt {
        let clauses = orelse.map(|body| ast::ElifElseClause {
            range: self.range,
            node_index: AtomicNodeIndex::default(),
            test: None,
            body: body.into_iter().collect(),
        });
        Stmt::If(ast::StmtIf {
            node_index: AtomicNodeIndex::default(),
            range: self.range,
            test: Box::new(test),
            body: body.into_iter().collect(),
            elif_else_clauses: clauses.into_iter().collect(),
        })
    }
}

/// The value of a starred part, which is what Python evaluates.
fn unstarred(part: &mut Expr) -> &mut Expr {
    match part {
        Expr::Starred(each) => &mut each.value,
        other => other,
    }
}
