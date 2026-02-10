use crate::Span;

/// Which grammar the source follows. Scripts and config files share one
/// lexer; only their keywords differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Script,
    Config,
}

/// Token kinds, named after the tokens of the official ANTLR grammar
/// (`ScriptLexer.g4` and `ConfigLexer.g4`).
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
    GStringExprStart,
    GStringEnd,

    // Groovy keywords kept by the strict syntax
    As,
    Def,
    In,
    BuiltInPrimitiveType,
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

    // separators
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    Semi,
    Comma,
    Dot,

    // operators
    Assign,
    Gt,
    Lt,
    Not,
    BitNot,
    Question,
    Colon,
    Equal,
    Le,
    Ge,
    NotEqual,
    And,
    Or,
    Add,
    Sub,
    Mul,
    Div,
    BitAnd,
    BitOr,
    Xor,
    Mod,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    AndAssign,
    OrAssign,
    XorAssign,
    ModAssign,
    LShiftAssign,
    RShiftAssign,
    URShiftAssign,
    ElvisAssign,
    PowerAssign,

    // identifiers
    CapitalizedIdentifier,
    Identifier,

    /// A line break or a comment.
    Nl,
    /// A character that starts no token.
    Unexpected,
    Eof,
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
        (">>>=", URShiftAssign),
        ("..<", RangeExclusiveRight),
        ("==~", RegexMatch),
        ("<=>", Spaceship),
        ("<<=", LShiftAssign),
        (">>=", RShiftAssign),
        ("**=", PowerAssign),
        ("..", RangeInclusive),
        ("*.", SpreadDot),
        ("?.", SafeDot),
        ("?:", Elvis),
        ("=~", RegexFind),
        ("**", Power),
        ("->", Arrow),
        ("==", Equal),
        ("<=", Le),
        (">=", Ge),
        ("!=", NotEqual),
        ("&&", And),
        ("||", Or),
        ("+=", AddAssign),
        ("-=", SubAssign),
        ("*=", MulAssign),
        ("/=", DivAssign),
        ("&=", AndAssign),
        ("|=", OrAssign),
        ("^=", XorAssign),
        ("%=", ModAssign),
        ("?=", ElvisAssign),
        ("(", LParen),
        (")", RParen),
        ("{", LBrace),
        ("}", RBrace),
        ("[", LBrack),
        ("]", RBrack),
        (";", Semi),
        (",", Comma),
        (".", Dot),
        ("=", Assign),
        (">", Gt),
        ("<", Lt),
        ("!", Not),
        ("~", BitNot),
        ("?", Question),
        (":", Colon),
        ("+", Add),
        ("-", Sub),
        ("*", Mul),
        ("/", Div),
        ("&", BitAnd),
        ("|", BitOr),
        ("^", Xor),
        ("%", Mod),
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
            BuiltInPrimitiveType
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
