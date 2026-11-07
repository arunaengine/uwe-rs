//! The lowered tree: the syntax tree after name resolution, which the interpreter runs.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::collections::HashMap;
use std::rc::Rc;

use nextflow_parser::Span;
pub use nextflow_parser::ast::Access;

use crate::value::{Operator, Relation, Value};

/// A variable slot of one call of a function or closure.
pub type Slot = usize;

/// A config local shared with closures after its declaring block finishes.
#[derive(Debug)]
pub struct Binding {
    pub value: std::cell::RefCell<Value>,
    pub class: Option<Rc<str>>,
}

/// Where a variable lives while code runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// A variable of the running function or closure.
    Local(Slot),
    /// A variable of an enclosing function or closure, captured when the closure was made.
    Captured(usize),
    /// A variable of the script that the top-level statements of legacy syntax declare or
    /// assign, in `Module::variables`.
    Module(usize),
}

/// The functions and processes of one script file.
#[derive(Debug, Default)]
pub struct Module {
    /// Whether the script sets `nextflow.enable.types = true`: its processes are typed, and
    /// calls of them give a channel, or a record of channels by output name.
    pub types: bool,
    pub functions: Vec<Rc<Code>>,
    /// Function indexes by name; Groovy allows several functions with one name.
    pub function_names: HashMap<Rc<str>, Vec<usize>>,
    pub processes: Vec<Rc<Process>>,
    pub workflows: Vec<Rc<Workflow>>,
    pub includes: Vec<Include>,
    /// `params.a.b = value` statements in load order; each value is a function without
    /// parameters.
    pub parameters: Vec<(Vec<Rc<str>>, Rc<Code>)>,
    /// The typed `params { }` block, in order.
    pub typed_parameters: Vec<TypedParameter>,
    /// The workflow outputs that the `output { }` block declares.
    pub outputs: Vec<OutputDeclaration>,
    /// What each include refers to: the included script and the name there. The module loader
    /// fills this in once the included scripts are loaded.
    pub links: std::cell::RefCell<HashMap<Rc<str>, Link>>,
    /// The links of the conditional includes, by index in `includes`, which their statements
    /// put into `links` when they run; the loader fills this in.
    pub conditional_links: std::cell::RefCell<HashMap<usize, Link>>,
    /// The directory of the script file, which `moduleDir` gives; the loader sets it.
    pub directory: std::cell::RefCell<Option<Rc<str>>>,
    /// The text of the file, which config output needs to show closures and error messages
    /// need for positions; the loaders set it.
    pub source: std::cell::RefCell<Option<Rc<str>>>,
    /// The plugin, like `nf-schema`, when this stands for the functions of a plugin, which the
    /// host provides.
    pub plugin: Option<Rc<str>>,
    /// The file of the script, for error messages; the loader sets it.
    pub path: std::cell::RefCell<Option<Rc<str>>>,
    /// Legacy syntax: the statements of a script that also declares functions, processes or
    /// workflows, which run when the script is loaded.
    pub initializer: Option<Rc<Code>>,
    /// The variables that the initializer declares or assigns, which all code of the script
    /// can read, as the legacy parser allows.
    pub variables: Vec<Rc<std::cell::RefCell<Value>>>,
    /// Legacy syntax that lowering accepts with the meaning of the legacy parser, with the
    /// errors of strict syntax.
    pub legacy: Vec<nextflow_parser::Diagnostic>,
    /// The Groovy classes of the file, as the `lib/` directory of older pipelines holds them,
    /// nested classes included.
    pub classes: Vec<Rc<ClassDef>>,
    /// The Groovy classes of the pipeline, which every script can use; the loader sets it.
    pub library: std::cell::RefCell<Option<Rc<Library>>>,
    /// Legacy syntax: the params of a script that an include with `addParams(...)` or
    /// `params(...)` loads, set when the include runs; `None` for the params of the run.
    pub params: std::cell::RefCell<Option<Value>>,
    /// Whether the statements of the script ran, which they do once.
    pub started: std::cell::Cell<bool>,
}

/// A Groovy class or enum, which only the legacy parser accepts.
#[derive(Debug)]
pub struct ClassDef {
    /// The name, after the names of the classes it is nested in, like `Constants.RunMode`.
    pub name: Rc<str>,
    pub enumeration: bool,
    /// The constants of an enum, in order.
    pub constants: Vec<Rc<str>>,
    pub fields: Vec<FieldDef>,
    pub methods: Vec<MethodDef>,
    pub constructors: Vec<MethodDef>,
    /// The names of the classes nested in this one, as `name` gives them.
    pub nested: Vec<Rc<str>>,
    pub span: Span,
}

#[derive(Debug)]
pub struct FieldDef {
    pub name: Rc<str>,
    pub is_static: bool,
    /// The simple name of the declared type, which converts the value; `None` for `def`.
    pub class: Option<Rc<str>>,
    /// The initial value, as code without parameters.
    pub value: Option<Rc<Code>>,
}

#[derive(Debug)]
pub struct MethodDef {
    pub name: Rc<str>,
    pub is_static: bool,
    /// Whether the last parameter takes the remaining arguments as a list.
    pub varargs: bool,
    /// The simple names of the parameter types, `None` for an untyped parameter, which Groovy
    /// checks to choose among methods of one name.
    pub types: Vec<Option<Rc<str>>>,
    /// The simple name of the return type, which converts the result; `None` for `def`.
    pub returns: Option<Rc<str>>,
    /// The code. A method of the objects takes the object, `this`, as its first parameter.
    pub code: Rc<Code>,
}

/// The Groovy classes that the scripts of a run can use, by name, with their static state.
#[derive(Debug, Default)]
pub struct Library {
    pub classes: std::cell::RefCell<HashMap<Rc<str>, Rc<LibraryClass>>>,
}

/// A class of the library with the script it belongs to and its state.
#[derive(Debug)]
pub struct LibraryClass {
    pub def: Rc<ClassDef>,
    pub module: Rc<Module>,
    /// The static fields, from the first time the class is used.
    pub statics: std::cell::RefCell<Option<indexmap::IndexMap<Rc<str>, Value>>>,
    /// The constants of an enum, and the class itself as a value, made once.
    pub values: std::cell::RefCell<Option<(Vec<Value>, Value)>>,
}

/// A param of the typed `params { }` block, like `input: String? = null`.
#[derive(Debug)]
pub struct TypedParameter {
    pub name: Rc<str>,
    /// The simple name of the declared type, if any.
    pub class: Option<Rc<str>>,
    /// Whether the type ends with `?`, so the param may stay `null`.
    pub nullable: bool,
    pub default: Option<Rc<Code>>,
    pub span: Span,
}

/// The script and name that an included name refers to.
#[derive(Clone, Debug)]
pub struct Link {
    pub module: Rc<Module>,
    pub name: Rc<str>,
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
    /// Legacy syntax: whether the include is inside an `if` of the script, so that it only
    /// takes effect when its statement runs.
    pub conditional: bool,
    /// Legacy syntax: whether the include gives the script params with `addParams(...)` or
    /// `params(...)`, so that the script loads again for it, and starts when the include runs.
    pub params: bool,
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
    /// For each emit, whether it only names a variable. After a `return` in the body,
    /// Nextflow still finds such an emit, but not one with a value of its own.
    pub bare_emits: Vec<bool>,
    /// The `publish:` assignments of the entry workflow, which run after its body.
    pub publishers: Vec<(Rc<str>, Expression)>,
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
    /// The inputs and the variables the sections assign without `def`, by name, for the
    /// names that config closures read when a task runs.
    pub variables: Vec<(Rc<str>, Slot)>,
    /// The variables that the `script:` or `stub:` section declares by assigning them. Only
    /// the section that runs sets them, and reading one that it did not set is an error.
    pub section_variables: Vec<(Rc<str>, Slot)>,
    /// Legacy syntax: `if` statements among the directives, like a check that exits for
    /// some profiles, which run for each task before the directives.
    pub setup: Vec<Statement>,
    /// The `env` directives of the `stage:` section of a typed process: each variable with
    /// the code of its value.
    pub stage_env: Vec<(Rc<str>, Expression)>,
    /// The `stdin` directive of the `stage:` section of a typed process.
    pub stage_stdin: Option<Expression>,
    /// The indexes of the inputs of a typed process whose type is not nullable, like `x: Int`
    /// but not `x: Int?`.
    pub non_null: Vec<usize>,
    /// What makes the process an agent, whose `script:` is the prompt to a language model.
    pub agent: Option<Agent>,
    pub span: Span,
}

/// An `agent` definition: a task whose answer comes from a language model.
#[derive(Debug)]
pub struct Agent {
    /// The names of the values of each input, for the input JSON: the name of the input, or
    /// the fields of a `record(...)` or `tuple(...)` input.
    pub inputs: Vec<Vec<Rc<str>>>,
    /// The typed output, which the model answers as JSON; `None` for a text answer.
    pub structured: Option<Structured>,
}

/// A typed agent output, like `result: Classification`.
#[derive(Debug)]
pub struct Structured {
    pub name: Rc<str>,
    pub kind: AnswerType,
}

/// A type that an agent can answer.
#[derive(Clone, Debug, PartialEq)]
pub enum AnswerType {
    Boolean,
    Integer,
    Float,
    Text,
    Path,
    List(Box<AnswerType>),
    Record(Vec<AnswerField>),
}

/// A field of a record type that an agent answers; an optional one may be missing.
#[derive(Clone, Debug, PartialEq)]
pub struct AnswerField {
    pub name: Rc<str>,
    pub kind: AnswerType,
    pub optional: bool,
}

/// A workflow output of the `output { }` block, with its options, like `path`, `mode` and
/// `enabled`, each as code that gives its value.
#[derive(Debug)]
pub struct OutputDeclaration {
    pub name: Rc<str>,
    pub options: Vec<(Rc<str>, Rc<Code>)>,
    /// The options of its `index { }` section, like `path`, `header` and `sep`.
    pub index: Vec<(Rc<str>, Rc<Code>)>,
}

/// A process directive, like `cpus 2` or `tag "${meta.id}"`, evaluated for each task.
#[derive(Debug)]
pub struct Directive {
    pub name: Rc<str>,
    pub arguments: Arguments,
    /// Legacy syntax: the condition of the `if` statements that the directive is in, like
    /// `container` in `if (workflow.containerEngine == 'singularity') { ... }`.
    pub guard: Option<Expression>,
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
        /// Whether text values are paths, as for `path`. The legacy `file` qualifier writes
        /// every value that is not a path into a file of its own instead.
        coerce: bool,
        /// Whether several files are a plain list, as typed inputs give them, instead of a
        /// list that prints its names separated by blanks.
        listed: bool,
    },
    /// `env(NAME)`
    Env(Rc<str>),
    Stdin,
    Tuple(Vec<Input>),
    /// `record(a: A, b: B)` of a typed process, which binds the fields of a record by name.
    Record(Vec<(Rc<str>, Input)>),
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
    /// The value of a typed output, computed after the task from its parts, which
    /// `ExpressionKind::TaskOutput` reads.
    pub typed: Option<Rc<Expression>>,
    /// Whether the output only feeds its topic, as an entry of a `topic:` section does.
    pub hidden: bool,
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
    /// `file(pattern)` or `files(pattern)` of a typed output: one file or a set of files.
    Files {
        pattern: Expression,
        options: Arguments,
        single: bool,
    },
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
    pub returns: Option<Rc<str>>,
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
    pub class: Option<Rc<str>>,
    pub slot: Slot,
    pub default: Option<Expression>,
    /// Whether the parameter takes the remaining arguments as a list, as Groovy does for a last
    /// parameter of an array type, like `String... rest` or `... rest`.
    pub varargs: bool,
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
        classes: Vec<Option<Rc<str>>>,
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
        classes: Vec<Option<Rc<str>>>,
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
    /// `for (x in items)` of legacy code: each element goes to the slot in turn.
    ForIn {
        slot: Slot,
        class: Option<Rc<str>>,
        items: Expression,
        body: Vec<Statement>,
    },
    /// `while` and the classic `for` of legacy code: `update` runs after each round.
    Loop {
        condition: Option<Expression>,
        update: Vec<Statement>,
        body: Vec<Statement>,
    },
    /// `switch` of legacy code: runs the statements from the first case that takes the
    /// subject, or else from the `default:` case, to a `break` or the end.
    Switch {
        subject: Expression,
        cases: Vec<SwitchCase>,
    },
    Break,
    Continue,
    /// Legacy syntax: an include inside an `if` of the script or inside a workflow, which
    /// links the includes with these indexes in `Module::includes` when it runs. With
    /// `addParams(...)`, `params` is the `IncludeParams` statement that runs first.
    Include {
        indexes: Vec<usize>,
        params: Option<Box<Statement>>,
    },
    /// Legacy syntax: `params.a.b = value` among the statements of a script, which sets the
    /// param when nothing, like the config or the command line, set it before.
    SetParam {
        path: Vec<Rc<str>>,
        value: Expression,
    },
    /// Legacy syntax: `include { ... } from '...' addParams(...)`, which gives the script of
    /// the includes with these indexes its params, with the params of this script when `add`,
    /// and then starts it.
    IncludeParams {
        indexes: Vec<usize>,
        add: bool,
        arguments: Arguments,
    },
}

#[derive(Debug)]
pub struct SwitchCase {
    pub values: Vec<Expression>,
    pub default: bool,
    pub body: Vec<Statement>,
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
    Binding(Rc<Binding>),
    TypedVariable {
        place: Place,
        class: Rc<str>,
    },
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
    /// A part of a typed output, like `file('out.txt')`, by its place in `Output::parts`.
    TaskOutput(usize),
    /// `null`, a boolean, a number or a string.
    Literal(Value),
    Binding(Rc<Binding>),
    Variable(Place),
    /// A built-in constant of the current scope, like `params`, `workflow` or `task`.
    Builtin(Rc<str>),
    /// A process or workflow used as a value, as in `FASTQC.out.html`.
    Definition(Rc<str>),
    /// A name that only a running task can resolve, as in a config closure like
    /// `ext.prefix = { meta.id }`: a task variable, a global like `params`, or an environment
    /// variable.
    Dynamic(Rc<str>),
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
        /// Whether Nextflow calls the function by reflection, which wraps what it throws in an
        /// `InvocationTargetException`: an included function, or one called from a workflow or
        /// process.
        reflective: bool,
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
    /// Legacy syntax: `left === right`, Java's identity, or `!==` when `negated`.
    Identical {
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
    /// `source.tap { x }` inside a chain of a workflow: `x` gets the channel, which passes on.
    Tap {
        source: Box<Expression>,
        target: Place,
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
    /// A class of the library of the pipeline, by name, as in `Utils.method()`.
    LibraryClass(Rc<str>),
    /// `x++` and `x--` of legacy code; the value is the one before.
    Step {
        target: Box<Target>,
        increment: bool,
    },
    /// An assignment used as a value in legacy code, like `a = b = c`.
    Assign {
        target: Box<Target>,
        update: Option<Update>,
        value: Box<Expression>,
    },
    /// `*items` as a call argument of legacy code, which passes the elements.
    Spread(Box<Expression>),
    /// `target."$name"` of legacy code.
    DynamicProperty {
        target: Box<Expression>,
        name: Box<Expression>,
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
