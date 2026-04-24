//! Syntax tree for Nextflow scripts and config files, as written in the source.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::Span;

/// A name with its position. A name written as a string, like `x.'**'`, holds the decoded text.
#[derive(Clone, Debug, PartialEq)]
pub struct Name {
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Script {
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// `nextflow.enable.dsl = 2`; the name leaves out `nextflow`.
    FeatureFlag {
        name: Vec<Name>,
        value: Expression,
        span: Span,
    },
    Include(Include),
    /// `import a.b.C`, `import a.b.C as D` or `import static a.b.C.member`, which only the
    /// legacy parser accepts.
    Import {
        name: QualifiedName,
        /// The name the import gives, if `as` gives one.
        alias: Option<Name>,
        /// `import static`
        member: bool,
        span: Span,
    },
    /// `params { ... }`
    Parameters {
        parameters: Vec<ParameterDeclaration>,
        span: Span,
    },
    /// `params.a.b = value`; the path leaves out `params`.
    LegacyParameter {
        path: Vec<Name>,
        value: Expression,
        span: Span,
    },
    Record {
        name: Name,
        fields: Vec<TypedName>,
        span: Span,
    },
    Enum {
        name: Name,
        variants: Vec<Name>,
        span: Span,
    },
    Process(Box<Process>),
    Workflow(Box<Workflow>),
    /// `output { ... }`
    Output {
        targets: Vec<OutputTarget>,
        span: Span,
    },
    Function(Box<Function>),
    Agent(Box<Agent>),
    /// A Groovy class, as the `lib/` directory of older pipelines holds them; only the legacy
    /// parser accepts one.
    Class(Box<Class>),
    Statement(Statement),
}

/// A Groovy class or enum.
#[derive(Clone, Debug, PartialEq)]
pub struct Class {
    pub name: Name,
    /// `enum` rather than `class`.
    pub enumeration: bool,
    /// The constants of an enum, in order.
    pub constants: Vec<Name>,
    pub members: Vec<Member>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Member {
    /// A field like `static List NAMES = ['a']`; several names may share one declaration.
    Field {
        name: Name,
        is_static: bool,
        annotation: Option<Type>,
        value: Option<Expression>,
        span: Span,
    },
    /// A method; a constructor has the name of its class and no return type.
    Method {
        function: Function,
        is_static: bool,
        /// Whether the last parameter takes the remaining arguments, as in `String... names`.
        varargs: bool,
    },
    /// A nested class or enum.
    Class(Box<Class>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Include {
    pub names: Vec<IncludeName>,
    pub source: Name,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IncludeName {
    pub name: Name,
    pub alias: Option<Name>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ParameterDeclaration {
    Parameter {
        name: Name,
        annotation: Option<Type>,
        default: Option<Expression>,
    },
    Statement(Statement),
}

/// `name` or `name: Type`.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedName {
    pub name: Name,
    pub annotation: Option<Type>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Process {
    pub name: Name,
    /// Statements before the first section label.
    pub directives: Vec<Statement>,
    pub inputs: Vec<ProcessInput>,
    pub stage: Vec<Statement>,
    pub outputs: Vec<NamedOutput>,
    pub topics: Vec<Statement>,
    pub when: Option<Expression>,
    pub body: Option<ProcessBody>,
    pub stub: Option<Block>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyKind {
    Script,
    Shell,
    Exec,
    /// A process body without any section label.
    Implicit,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessBody {
    pub kind: BodyKind,
    pub block: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProcessInput {
    /// `name` or `name: Type`. A bare name can also be a legacy input like `stdin`.
    Named(TypedName),
    /// `record(a: A, b: B)`
    Record(Vec<TypedName>),
    /// `tuple(a: A, b: B)`
    Tuple(Vec<TypedName>),
    /// A legacy input like `tuple val(meta), path(reads)`.
    Statement(Statement),
}

/// A process output, workflow emit or workflow publisher.
#[derive(Clone, Debug, PartialEq)]
pub enum NamedOutput {
    /// `name`, `name: Type` or `name = value`.
    Named {
        target: TypedName,
        value: Option<Expression>,
    },
    Statement(Statement),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Workflow {
    /// `None` for the entry workflow.
    pub name: Option<Name>,
    pub takes: Vec<WorkflowTake>,
    pub main: Block,
    pub emits: Vec<NamedOutput>,
    pub publishers: Vec<NamedOutput>,
    pub on_complete: Option<Block>,
    pub on_error: Option<Block>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WorkflowTake {
    Named(TypedName),
    Statement(Statement),
}

#[derive(Clone, Debug, PartialEq)]
pub enum OutputTarget {
    /// `name { ... }` or `name: Type { ... }`
    Target {
        name: Name,
        annotation: Option<Type>,
        body: Block,
    },
    Statement(Statement),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    pub name: Name,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<Type>,
    pub body: Block,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    pub name: Name,
    pub annotation: Option<Type>,
    pub default: Option<Expression>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub name: Name,
    pub directives: Vec<Statement>,
    pub inputs: Vec<ProcessInput>,
    pub outputs: Vec<NamedOutput>,
    pub prompt: Block,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub statements: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}

impl Statement {
    /// Whether the statement declares or assigns a plain variable, like `def x = 1` or
    /// `x = 1`, which legacy processes allow among their sections.
    pub fn is_variable_statement(&self) -> bool {
        match &self.kind {
            StatementKind::Declaration { .. } => true,
            StatementKind::Assign { target, .. } => {
                matches!(target.kind, ExpressionKind::Variable(_))
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum StatementKind {
    Expression(Expression),
    Assign {
        target: Expression,
        operator: AssignOperator,
        value: Expression,
    },
    /// `(a, b) = value`
    MultipleAssign {
        names: Vec<Name>,
        value: Expression,
    },
    /// `def x = 1`, `def x: Integer`, `def (a, b) = pair` or `String x = ''`.
    Declaration {
        names: Vec<TypedName>,
        legacy_type: Option<Type>,
        initializer: Option<Expression>,
        destructure: bool,
    },
    If {
        condition: Expression,
        then: Block,
        otherwise: Option<Block>,
    },
    Try {
        body: Block,
        catches: Vec<Catch>,
    },
    Return(Option<Expression>),
    Throw(Expression),
    Assert {
        condition: Expression,
        message: Option<Expression>,
    },
    /// `label: statement`, only in closures, as used by operators like `branch`.
    Labeled {
        label: Name,
        statement: Box<Statement>,
    },
    /// `for (x in items) body`, which only the legacy parser accepts.
    ForIn {
        variable: Name,
        items: Expression,
        body: Block,
    },
    /// `for (init; condition; update) body`, which only the legacy parser accepts.
    For {
        init: Option<Box<Statement>>,
        condition: Option<Expression>,
        update: Vec<Statement>,
        body: Block,
    },
    /// `while (condition) body`, which only the legacy parser accepts.
    While {
        condition: Expression,
        body: Block,
    },
    /// `switch (subject) { case value: ... default: ... }`, which only the legacy parser
    /// accepts.
    Switch {
        subject: Expression,
        cases: Vec<SwitchCase>,
    },
    /// `break`, which only the legacy parser accepts.
    Break,
    /// `continue`, which only the legacy parser accepts.
    Continue,
    Empty,
}

/// The statements after one or more `case value:` or `default:` labels of a `switch`.
#[derive(Clone, Debug, PartialEq)]
pub struct SwitchCase {
    pub values: Vec<Expression>,
    /// Whether one of the labels is `default:`.
    pub default: bool,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Catch {
    pub name: Name,
    pub types: Vec<QualifiedName>,
    pub body: Block,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssignOperator {
    Assign,
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
    Power,
    BitAnd,
    BitOr,
    Xor,
    LeftShift,
    RightShift,
    UnsignedShift,
    Elvis,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expression {
    pub kind: ExpressionKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExpressionKind {
    Variable(String),
    Literal(Literal),
    GString(Vec<GStringPart>),
    /// `new Foo(arguments)`
    New {
        class: Type,
        arguments: Vec<Argument>,
    },
    Parenthesized(Box<Expression>),
    Closure(Box<Closure>),
    List(Vec<Expression>),
    Map(Vec<MapEntry>),
    /// A primitive type name used as a value, like `int`.
    PrimitiveType(String),
    Property {
        target: Box<Expression>,
        name: Name,
        access: Access,
    },
    /// `name(...)`, `target.name(...)`, `name { }` or, without parentheses, `name a, key: b`.
    MethodCall {
        target: Option<Box<Expression>>,
        name: Name,
        access: Access,
        arguments: Vec<Argument>,
        parentheses: bool,
    },
    /// Calling a value that is not a plain name, like `f()(x)`.
    Call {
        callee: Box<Expression>,
        arguments: Vec<Argument>,
    },
    Index {
        target: Box<Expression>,
        index: Vec<Expression>,
    },
    Unary {
        operator: UnaryOperator,
        operand: Box<Expression>,
    },
    Binary {
        operator: BinaryOperator,
        left: Box<Expression>,
        right: Box<Expression>,
    },
    /// `value as Type`
    Cast {
        operand: Box<Expression>,
        target_type: Type,
    },
    InstanceOf {
        operand: Box<Expression>,
        target_type: Type,
        negated: bool,
    },
    Ternary {
        condition: Box<Expression>,
        then: Box<Expression>,
        otherwise: Box<Expression>,
    },
    /// `value ?: fallback`
    Elvis {
        value: Box<Expression>,
        fallback: Box<Expression>,
    },
    /// `x++` or `x--`, which only the legacy parser accepts; the value is the one before.
    Postfix {
        operand: Box<Expression>,
        increment: bool,
    },
    /// `*items` as a call argument or list element, which only the legacy parser accepts.
    Spread(Box<Expression>),
    /// `target."$name"`: a property whose name is computed, which only the legacy parser
    /// accepts.
    DynamicProperty {
        target: Box<Expression>,
        name: Box<Expression>,
    },
    /// An assignment used as a value, like `f(x = 1)` or `a = b = c`, which only the legacy
    /// parser accepts.
    Assign {
        target: Box<Expression>,
        operator: AssignOperator,
        value: Box<Expression>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    /// The source text, for example `10L` or `0x1F`, so the evaluator can pick the Groovy type.
    Integer(String),
    /// The source text, for example `1.5` or `2d`.
    Float(String),
    /// The decoded text.
    String(String),
    Boolean(bool),
    Null,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GStringPart {
    /// Decoded text.
    Text(String),
    /// `$name.path` or `${expression}`.
    Value(Expression),
}

/// How a property or method is reached: `.`, `?.`, `*.`, or the legacy `.@`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Normal,
    Safe,
    Spread,
    /// `.@name`, which reads a field or an XML attribute directly.
    Attribute,
}

/// A call argument; named arguments look like `key: value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Argument {
    pub name: Option<Name>,
    pub value: Expression,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Closure {
    /// `None` gives the implicit parameter `it`; `Some(vec![])` is `{ -> ... }`.
    pub parameters: Option<Vec<Parameter>>,
    pub body: Block,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MapEntry {
    pub key: MapKey,
    pub value: Expression,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MapKey {
    /// A bare name or keyword, which is a string key.
    Name(Name),
    /// Any other primary expression, like `'a'`, `1` or `(key)`.
    Expression(Expression),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOperator {
    Not,
    BitNot,
    Plus,
    Minus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOperator {
    Power,
    Multiply,
    Divide,
    Modulo,
    Add,
    Subtract,
    LeftShift,
    RightShift,
    UnsignedShift,
    RangeInclusive,
    RangeExclusiveRight,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    In,
    NotIn,
    Equal,
    NotEqual,
    Spaceship,
    RegexFind,
    RegexMatch,
    BitAnd,
    Xor,
    BitOr,
    And,
    Or,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Type {
    pub name: QualifiedName,
    pub arguments: Vec<TypeArgument>,
    /// `Type?`
    pub nullable: bool,
    /// Legacy array dimensions, as in `String[]`.
    pub dimensions: u32,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TypeArgument {
    Type(Type),
    /// `?`
    Wildcard,
}

/// A dotted name like `java.util.List`; primitive types have one part.
#[derive(Clone, Debug, PartialEq)]
pub struct QualifiedName {
    pub parts: Vec<Name>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub statements: Vec<ConfigStatement>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigStatement {
    /// `includeConfig 'path'`
    Include { path: Expression, span: Span },
    /// `a.b.c = value`
    Assign {
        path: Vec<Name>,
        value: Expression,
        span: Span,
    },
    /// `name { ... }`
    Block {
        name: Name,
        body: Vec<ConfigStatement>,
        span: Span,
    },
    /// A block of calls, like `plugins { id 'nf-schema' }`.
    Apply {
        name: Name,
        calls: Vec<ConfigApply>,
        span: Span,
    },
    /// `withName: 'FOO' { ... }` or `withLabel: 'big' { ... }`
    Selector {
        kind: Name,
        target: Name,
        body: Vec<ConfigStatement>,
        span: Span,
    },
    // The statements below are legacy syntax: strict syntax rejects them, and iunctus runs
    // them with the meaning of the legacy parser (decision 14).
    /// `if (condition) { ... } else { ... }` around config statements.
    If {
        condition: Expression,
        then: Vec<ConfigStatement>,
        otherwise: Vec<ConfigStatement>,
        span: Span,
    },
    /// `try { ... } catch (Exception e) { ... }` around config statements.
    Try {
        body: Vec<ConfigStatement>,
        catches: Vec<ConfigCatch>,
        span: Span,
    },
    /// A local variable, like `def timestamp = new Date().format('yyyy')`.
    Declaration {
        names: Vec<Name>,
        value: Option<Expression>,
        /// `def (a, b) = pair`
        destructure: bool,
        span: Span,
    },
    /// A function, like `def check_max(obj, type) { ... }`.
    Function(Box<Function>),
    /// Any other statement, like `System.err.println('...')`.
    Statement(Statement),
}

/// `catch (Exception e) { ... }` of a legacy `try` in a config file.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigCatch {
    pub name: Name,
    /// Empty for `catch (e)`, which catches every exception.
    pub types: Vec<QualifiedName>,
    pub body: Vec<ConfigStatement>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConfigApply {
    pub name: Name,
    pub arguments: Vec<Argument>,
    pub span: Span,
}
