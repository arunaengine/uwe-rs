//! Token kinds, keywords and operators of the Nextflow grammar.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::Span;

/// Which grammar the source follows. Scripts and config files share one
/// lexer; only their keywords differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Script,
    Config,
}

/// Token kinds of the official ANTLR grammars (`ScriptLexer.g4`, `ConfigLexer.g4`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // literals
    StringLiteral,
    IntegerLiteral,
    FloatingPointLiteral,
    BooleanLiteral,
    NullLiteral,

    // interpolated strings; `triple` marks `"""` strings
    GStringBegin {
        triple: bool,
    },
    GStringText,
    GStringPath,
    InterpolationStart,
    GStringEnd,

    // Groovy keywords kept by the strict syntax
    As,
    Def,
    In,
    PrimitiveType,
    Assert,
    Catch,
    Else,
    Enum,
    If,
    Import,
    Instanceof,
    New,
    Record,
    Return,
    Throw,
    Try,

    // Nextflow keywords (scripts)
    Nextflow,
    Params,
    Include,
    From,
    Agent,
    Prompt,
    Process,
    Exec,
    Input,
    Output,
    Script,
    Shell,
    Stage,
    Stub,
    Topic,
    Tuple,
    When,
    Workflow,
    Emit,
    Main,
    OnComplete,
    OnError,
    Publish,
    Take,

    // Nextflow keywords (config files)
    IncludeConfig,

    // operators
    RangeInclusive,
    RangeExclusiveRight,
    SpreadDot,
    SafeDot,
    Elvis,
    RegexFind,
    RegexMatch,
    Power,
    Spaceship,
    Arrow,
    NotInstanceof,
    NotIn,

    /// `@` after a dot, as in the legacy `node.@name`.
    At,

    // separators
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Semicolon,
    Comma,
    Dot,

    // operators
    Assign,
    Greater,
    Less,
    Not,
    BitNot,
    Question,
    Colon,
    Equal,
    LessEqual,
    GreaterEqual,
    NotEqual,
    And,
    Or,
    Add,
    Subtract,
    Multiply,
    Divide,
    BitAnd,
    BitOr,
    Xor,
    Modulo,
    AddAssign,
    SubtractAssign,
    MultiplyAssign,
    DivideAssign,
    BitAndAssign,
    BitOrAssign,
    XorAssign,
    ModuloAssign,
    LeftShiftAssign,
    RightShiftAssign,
    UnsignedShiftAssign,
    ElvisAssign,
    PowerAssign,

    // identifiers
    CapitalizedIdentifier,
    Identifier,

    /// A line break or a comment.
    Newline,
    /// A character that starts no token.
    Unexpected,
    EndOfFile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// Operators and separators, longest first so that the first match is the
/// longest match.
pub(crate) const OPERATORS: &[(&str, TokenKind)] = {
    use TokenKind::*;
    &[
        (">>>=", UnsignedShiftAssign),
        ("..<", RangeExclusiveRight),
        ("==~", RegexMatch),
        ("<=>", Spaceship),
        ("<<=", LeftShiftAssign),
        (">>=", RightShiftAssign),
        ("**=", PowerAssign),
        ("..", RangeInclusive),
        ("*.", SpreadDot),
        ("?.", SafeDot),
        ("?:", Elvis),
        ("=~", RegexFind),
        ("**", Power),
        ("->", Arrow),
        ("==", Equal),
        ("<=", LessEqual),
        (">=", GreaterEqual),
        ("!=", NotEqual),
        ("&&", And),
        ("||", Or),
        ("+=", AddAssign),
        ("-=", SubtractAssign),
        ("*=", MultiplyAssign),
        ("/=", DivideAssign),
        ("&=", BitAndAssign),
        ("|=", BitOrAssign),
        ("^=", XorAssign),
        ("%=", ModuloAssign),
        ("?=", ElvisAssign),
        ("(", LeftParen),
        (")", RightParen),
        ("{", LeftBrace),
        ("}", RightBrace),
        ("[", LeftBracket),
        ("]", RightBracket),
        (";", Semicolon),
        (",", Comma),
        (".", Dot),
        ("=", Assign),
        (">", Greater),
        ("<", Less),
        ("!", Not),
        ("~", BitNot),
        ("?", Question),
        (":", Colon),
        ("+", Add),
        ("-", Subtract),
        ("*", Multiply),
        ("/", Divide),
        ("&", BitAnd),
        ("|", BitOr),
        ("^", Xor),
        ("%", Modulo),
    ]
};

/// The keyword for a word, if it is one in the given dialect.
pub fn keyword(word: &str, dialect: Dialect) -> Option<TokenKind> {
    use TokenKind::*;
    let kind = match word {
        "as" => As,
        "def" => Def,
        "in" => In,
        "boolean" | "char" | "byte" | "short" | "int" | "long" | "float" | "double" => {
            PrimitiveType
        }
        "assert" => Assert,
        "catch" => Catch,
        "else" => Else,
        "if" => If,
        "instanceof" => Instanceof,
        "new" => New,
        "return" => Return,
        "throw" => Throw,
        "try" => Try,
        "true" | "false" => BooleanLiteral,
        "null" => NullLiteral,
        _ => match dialect {
            Dialect::Script => script_keyword(word)?,
            Dialect::Config => match word {
                "includeConfig" => IncludeConfig,
                _ => return None,
            },
        },
    };
    Some(kind)
}

fn script_keyword(word: &str) -> Option<TokenKind> {
    use TokenKind::*;
    Some(match word {
        "enum" => Enum,
        "import" => Import,
        "record" => Record,
        "nextflow" => Nextflow,
        "params" => Params,
        "include" => Include,
        "from" => From,
        "agent" => Agent,
        "prompt" => Prompt,
        "process" => Process,
        "exec" => Exec,
        "input" => Input,
        "output" => Output,
        "script" => Script,
        "shell" => Shell,
        "stage" => Stage,
        "stub" => Stub,
        "topic" => Topic,
        "tuple" => Tuple,
        "when" => When,
        "workflow" => Workflow,
        "emit" => Emit,
        "main" => Main,
        "onComplete" => OnComplete,
        "onError" => OnError,
        "publish" => Publish,
        "take" => Take,
        _ => return None,
    })
}
