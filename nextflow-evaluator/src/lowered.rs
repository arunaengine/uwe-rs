//! The lowered tree: the syntax tree after name resolution, which the interpreter runs.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use nextflow_parser::Span;
pub use nextflow_parser::ast::Access;

use crate::value::{Operator, Relation, Value};

/// A variable slot of one call of a function or closure.
pub type Slot = usize;

/// Where a variable lives while code runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// A variable of the running function or closure.
    Local(Slot),
    /// A variable of an enclosing function or closure, captured when the closure was made.
    Captured(usize),
}

/// The functions and processes of one script file.
#[derive(Debug, Default)]
pub struct Module {
    pub functions: Vec<Rc<Code>>,
    /// Function indexes by name; Groovy allows several functions with one name.
    pub function_names: HashMap<Rc<str>, Vec<usize>>,
    pub processes: Vec<Rc<Process>>,
    pub workflows: Vec<Rc<Workflow>>,
    pub includes: Vec<Include>,
    /// `params.a.b = value` statements in load order; each value is a function without
    /// parameters.
    pub parameters: Vec<(Vec<Rc<str>>, Rc<Code>)>,
}

/// What a name of a script stands for: a function, a process or a workflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefinitionKind {
    Function,
    Process,
    Workflow,
}

/// `include { NAME as ALIAS } from 'source'`.
#[derive(Debug)]
pub struct Include {
    /// The name in the included script.
    pub name: Rc<str>,
    /// The name in this script.
    pub alias: Rc<str>,
    pub source: Rc<str>,
    pub span: Span,
}

/// A workflow; the entry workflow has no name. Its takes, the variables its body assigns
/// without `def`, and its locals share one frame.
#[derive(Debug)]
pub struct Workflow {
    pub name: Option<Rc<str>>,
    pub slot_count: usize,
    pub takes: Vec<(Rc<str>, Slot)>,
    pub main: Vec<Statement>,
    pub emits: Vec<(Rc<str>, Expression)>,
    pub span: Span,
}

/// A process. Its inputs, the variables its script section sets without `def`, and the
/// locals of its sections share one frame, which is made anew for each task.
#[derive(Debug)]
pub struct Process {
    pub name: Rc<str>,
    pub slot_count: usize,
    pub directives: Vec<Directive>,
    pub inputs: Vec<Input>,
    pub outputs: Vec<Output>,
    pub when: Option<Expression>,
    pub body: Option<Body>,
    pub stub: Option<Vec<Statement>>,
    pub span: Span,
}

/// A process directive, like `cpus 2` or `tag "${meta.id}"`, evaluated for each task.
#[derive(Debug)]
pub struct Directive {
    pub name: Rc<str>,
    pub arguments: Arguments,
    pub span: Span,
}

#[derive(Debug)]
pub enum Input {
    /// `val(x)`
    Value {
        slot: Slot,
    },
    /// `path(x)` with options like `stageAs:`, or `path 'name'` without a variable.
    Path {
        slot: Option<Slot>,
        pattern: Option<Expression>,
        options: Arguments,
    },
    /// `env(NAME)`
    Env(Rc<str>),
    Stdin,
    Tuple(Vec<Input>),
    /// `each x`, which repeats the task for every element.
    Each(Box<Input>),
}

/// A process output: one part, or a `tuple` of parts, with `emit:`, `topic:` and `optional:`.
#[derive(Debug)]
pub struct Output {
    pub parts: Vec<OutputPart>,
    pub tuple: bool,
    pub emit: Option<Rc<str>>,
    pub topic: Option<Rc<str>>,
    pub optional: Option<Expression>,
    pub span: Span,
}

#[derive(Debug)]
pub enum OutputPart {
    Value(Expression),
    /// `path(pattern)` with options like `arity:` or `includeInputs:`.
    Path {
        pattern: Expression,
        options: Arguments,
    },
    Env(Rc<str>),
    /// `eval(command)`: the output of a command run after the script.
    Eval(Expression),
    Stdout,
}

/// The `script:`, `shell:` or `exec:` section of a process.
#[derive(Debug)]
pub struct Body {
    pub kind: nextflow_parser::ast::BodyKind,
    pub statements: Vec<Statement>,
}

/// The code of a function or closure.
#[derive(Debug)]
pub struct Code {
    pub name: Rc<str>,
    pub parameters: Vec<Parameter>,
    /// A closure without parameters has one optional parameter `it`, in slot 0.
    pub implicit_parameter: bool,
    pub slot_count: usize,
    /// For a closure: the enclosing variables it captures, in the order of `Place::Captured`.
    pub captures: Vec<Place>,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Debug)]
pub struct Parameter {
    pub name: Rc<str>,
    pub slot: Slot,
    pub default: Option<Expression>,
}

#[derive(Debug)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum StatementKind {
    Expression(Expression),
    /// `def x = value` or `def (a, b) = value`; without a value the variables are `null`.
    Declare {
        slots: Vec<Slot>,
        value: Option<Expression>,
        destructure: bool,
    },
    Assign {
        target: Target,
        value: Expression,
    },
    /// `x += y`, `x ?= y` and the other compound assignments.
    Update {
        target: Target,
        update: Update,
        value: Expression,
    },
    /// `(a, b) = value`
    Destructure {
        places: Vec<Place>,
        value: Expression,
    },
    If {
        condition: Expression,
        then: Vec<Statement>,
        otherwise: Vec<Statement>,
    },
    Try {
        body: Vec<Statement>,
        catches: Vec<Catch>,
    },
    Return(Option<Expression>),
    Throw(Expression),
    Assert {
        condition: Expression,
        message: Option<Expression>,
        /// The source text of the condition, for the message of a failed assert.
        text: Rc<str>,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum Update {
    Operator(Operator),
    /// `?=`
    Elvis,
}

#[derive(Debug)]
pub enum Target {
    Variable(Place),
    Property {
        target: Expression,
        name: Rc<str>,
    },
    Index {
        target: Expression,
        index: Expression,
    },
}

#[derive(Debug)]
pub struct Catch {
    pub slot: Slot,
    /// Simple class names; empty catches every exception.
    pub classes: Vec<Rc<str>>,
    pub body: Vec<Statement>,
}

#[derive(Debug)]
pub struct Expression {
    pub kind: ExpressionKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum ExpressionKind {
    /// `null`, a boolean, a number or a string.
    Literal(Value),
    Variable(Place),
    /// A built-in constant of the current scope, like `params`, `workflow` or `task`.
    Builtin(Rc<str>),
    /// A process or workflow used as a value, as in `FASTQC.out.html`.
    Definition(Rc<str>),
    /// A channel operator used as a value, as in `ch | view`.
    Operator(Rc<str>),
    /// A GString. Nextflow makes it a `String` unless it is nested in another GString.
    Interpolate {
        parts: Vec<Part>,
        keep_gstring: bool,
        /// In a task script, a staged input file is inserted with its special characters
        /// escaped, as Nextflow's `TaskCmdXformVisitor` arranges.
        escape_paths: bool,
    },
    List(Vec<Expression>),
    Map(Vec<(Expression, Expression)>),
    Range {
        from: Box<Expression>,
        to: Box<Expression>,
        exclusive: bool,
    },
    Closure(Rc<Code>),
    Property {
        target: Box<Expression>,
        name: Rc<str>,
        access: Access,
    },
    CallMethod {
        target: Box<Expression>,
        name: Rc<str>,
        access: Access,
        arguments: Arguments,
    },
    /// A call of a function of the script or a built-in function, by name.
    CallFunction {
        function: Function,
        arguments: Arguments,
    },
    /// Calling a value, like `f()(x)`.
    CallValue {
        callee: Box<Expression>,
        arguments: Arguments,
    },
    /// `Math.max(1, 2)` and other static methods of a known class.
    CallStatic {
        class: Rc<str>,
        name: Rc<str>,
        arguments: Arguments,
    },
    /// `Integer.MAX_VALUE` and other static fields of a known class.
    StaticProperty {
        class: Rc<str>,
        name: Rc<str>,
    },
    Index {
        target: Box<Expression>,
        index: Vec<Expression>,
    },
    Unary {
        operator: Unary,
        operand: Box<Expression>,
    },
    Binary {
        operator: Operator,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Relation {
        relation: Relation,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Equal {
        negated: bool,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Spaceship {
        left: Box<Expression>,
        right: Box<Expression>,
    },
    /// `item in container`
    In {
        negated: bool,
        item: Box<Expression>,
        container: Box<Expression>,
    },
    /// `=~` (`matches: false`) and `==~` (`matches: true`).
    Regex {
        matches: bool,
        text: Box<Expression>,
        pattern: Box<Expression>,
    },
    And {
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Or {
        left: Box<Expression>,
        right: Box<Expression>,
    },
    Ternary {
        condition: Box<Expression>,
        then: Box<Expression>,
        otherwise: Box<Expression>,
    },
    Elvis {
        value: Box<Expression>,
        fallback: Box<Expression>,
    },
    Cast {
        operand: Box<Expression>,
        class: Rc<str>,
    },
    InstanceOf {
        operand: Box<Expression>,
        class: Rc<str>,
        negated: bool,
    },
    /// `new Foo(arguments)`
    New {
        class: Rc<str>,
        arguments: Arguments,
    },
}

#[derive(Debug)]
pub enum Part {
    Text(Rc<str>),
    Value(Expression),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unary {
    Not,
    Negate,
    Positive,
    BitwiseNot,
}

/// Call arguments. Groovy passes named arguments as one map in first position.
#[derive(Debug, Default)]
pub struct Arguments {
    pub named: Vec<(Rc<str>, Expression)>,
    pub positional: Vec<Expression>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Function {
    /// A function of the same script, resolved by name and argument count when called.
    Script(Rc<str>),
    /// A built-in function of the current scope, like `file` or `println`.
    Builtin(Rc<str>),
    /// A process or workflow of the script, or an included name that is one.
    Definition(Rc<str>),
    /// A channel operator called without a receiver, as in `ch | map { ... }`.
    Operator(Rc<str>),
}
