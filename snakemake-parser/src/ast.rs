//! Syntax tree of a Snakefile: Python code from ruff plus the Snakemake statements.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use ruff_python_ast::{Arguments, Expr, Suite};
use ruff_text_size::{Ranged, TextRange, TextSize};

use crate::keyword::{GlobalKeyword, ModuleKeyword, RuleKeyword};

/// A parsed Snakefile. Each Snakemake statement stands in `python` as a `...` expression
/// statement at the statement's start offset; [`Snakefile::statement_at`] finds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Snakefile {
    pub python: Suite,
    /// Snakemake statements at any Python nesting depth, in source order.
    pub statements: Vec<Statement>,
}

impl Snakefile {
    /// The Snakemake statement that starts at `offset`, such as the start of a placeholder.
    pub fn statement_at(&self, offset: TextSize) -> Option<&Statement> {
        let index = self
            .statements
            .binary_search_by_key(&offset, |statement| statement.range().start())
            .ok()?;
        self.statements.get(index)
    }

    /// Rules and checkpoints in source order.
    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.statements
            .iter()
            .filter_map(|statement| match statement {
                Statement::Rule(rule) => Some(rule),
                _ => None,
            })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    /// A global keyword with its value, like `configfile: "config.yaml"` or `onstart:`.
    Global(Directive<GlobalKeyword>),
    Rule(Rule),
    UseRule(UseRule),
    Module(Module),
    Storage(Storage),
}

impl Statement {
    pub fn range(&self) -> TextRange {
        match self {
            Statement::Global(directive) => directive.range,
            Statement::Rule(rule) => rule.range,
            Statement::UseRule(rule) => rule.range,
            Statement::Module(module) => module.range,
            Statement::Storage(storage) => storage.range,
        }
    }
}

/// A keyword, a colon and a value, inside a rule, a module or at the top level.
#[derive(Clone, Debug, PartialEq)]
pub struct Directive<K> {
    pub keyword: K,
    pub keyword_range: TextRange,
    pub value: Value,
    pub range: TextRange,
}

/// The value after a keyword's colon, parsed the way parser.py's generated Python uses it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Call arguments, as for `input: "a.txt", b=f"{x}.txt"`.
    Arguments(Arguments),
    /// One expression, as for module settings like `snakefile: github(...)`.
    Expression(Box<Expr>),
    /// Python statements, as for `run:` and the `onstart:` handlers.
    Body(Suite),
    /// Rule names, as for `localrules: a, b` and `ruleorder: a > b`.
    Names(Vec<Name>),
}

/// A rule or a checkpoint.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub checkpoint: bool,
    /// Absent for anonymous rules, which may get a name from a `name:` directive.
    pub name: Option<Name>,
    pub docstrings: Vec<Expr>,
    pub directives: Vec<Directive<RuleKeyword>>,
    pub range: TextRange,
}

/// `use rule <rules> [from <module>] [exclude <rules>] [as <alias>] [with:]`.
#[derive(Clone, Debug, PartialEq)]
pub struct UseRule {
    /// Rule names, or a single `*` for every rule of the module.
    pub rules: Vec<Name>,
    pub module: Option<Name>,
    pub exclude: Vec<Name>,
    /// The new name or name pattern, like `other` or `module_*`.
    pub alias: Option<Name>,
    pub docstrings: Vec<Expr>,
    /// The directives of the `with:` block, if the statement has one.
    pub with: Option<Vec<Directive<RuleKeyword>>>,
    pub range: TextRange,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Module {
    /// The name after the keyword; a `name:` setting can give it instead.
    pub name: Option<Name>,
    pub settings: Vec<Directive<ModuleKeyword>>,
    pub range: TextRange,
}

/// `storage [<tag>]:` with the arguments of the storage provider.
#[derive(Clone, Debug, PartialEq)]
pub struct Storage {
    pub tag: Option<Name>,
    pub arguments: Arguments,
    pub range: TextRange,
}

/// A name as written in the source, such as a rule name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub range: TextRange,
}

impl Ranged for Name {
    fn range(&self) -> TextRange {
        self.range
    }
}
