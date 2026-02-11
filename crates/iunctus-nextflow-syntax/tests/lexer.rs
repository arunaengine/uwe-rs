use iunctus_nextflow_syntax::lexer::tokenize;
use iunctus_nextflow_syntax::token::{Dialect, TokenKind, TokenKind::*};

fn kinds_in(source: &str, dialect: Dialect) -> Vec<TokenKind> {
    let lexed = tokenize(source, dialect);
    assert!(
        lexed.errors.is_empty(),
        "unexpected errors: {:?}",
        lexed.errors
    );
    let mut kinds: Vec<_> = lexed.tokens.iter().map(|t| t.kind).collect();
    assert_eq!(kinds.pop(), Some(Eof));
    kinds
}

fn kinds(source: &str) -> Vec<TokenKind> {
    kinds_in(source, Dialect::Script)
}

fn texts(source: &str) -> Vec<&str> {
    let lexed = tokenize(source, Dialect::Script);
    lexed.tokens.iter().map(|t| t.span.text(source)).collect()
}

#[test]
fn simple_assignment() {
    assert_eq!(kinds("x = 1"), [Identifier, Assign, IntegerLiteral]);
}

#[test]
fn slash_is_regex_only_where_a_value_is_expected() {
    assert_eq!(kinds("a =~ /fo+/"), [Identifier, RegexFind, StringLiteral]);
    assert_eq!(
        kinds("a / b / c"),
        [Identifier, Div, Identifier, Div, Identifier]
    );
    assert_eq!(kinds("~/^a$/"), [BitNot, StringLiteral]);
    assert_eq!(
        kinds("(a) / 2"),
        [LParen, Identifier, RParen, Div, IntegerLiteral]
    );
}

#[test]
fn plain_and_interpolated_strings() {
    assert_eq!(kinds("\"hello\" 'x'"), [StringLiteral, StringLiteral]);
    assert_eq!(
        kinds("\"a $x b\""),
        [
            GStringBegin { triple: false },
            GStringText,
            GStringPath,
            GStringText,
            GStringEnd
        ]
    );
    assert_eq!(texts("\"$meta.id.x\"")[1], "$meta.id.x");
    assert_eq!(texts("\"${prefix}.bam\"")[4], ".bam");
}

#[test]
fn gstring_expressions_nest_brackets() {
    assert_eq!(
        kinds("\"${a.b { it }}\""),
        [
            GStringBegin { triple: false },
            GStringExprStart,
            Identifier,
            Dot,
            Identifier,
            LBrace,
            Identifier,
            RBrace,
            RBrace,
            GStringEnd
        ]
    );
}

#[test]
fn triple_quoted_script() {
    let source = "\"\"\"\necho \\$HOME ${task.cpus}\n\"\"\"";
    assert_eq!(
        kinds(source),
        [
            GStringBegin { triple: true },
            GStringText,
            GStringExprStart,
            Identifier,
            Dot,
            Identifier,
            RBrace,
            GStringText,
            GStringEnd
        ]
    );
}

#[test]
fn quote_runs_at_the_end_of_triple_strings() {
    assert_eq!(texts("\"\"\"a\"\"\"\" x"), ["\"\"\"a\"\"\"\"", "x", ""]);
    assert_eq!(texts("'''a''''' x"), ["'''a'''''", "x", ""]);
}

#[test]
fn numbers() {
    assert_eq!(
        kinds("1 1.5 .5 0x1F 10L 1e5 1G 1.0G 2d"),
        [
            IntegerLiteral,
            FloatingPointLiteral,
            FloatingPointLiteral,
            IntegerLiteral,
            IntegerLiteral,
            FloatingPointLiteral,
            IntegerLiteral,
            FloatingPointLiteral,
            FloatingPointLiteral
        ]
    );
    assert_eq!(kinds("2.GB"), [IntegerLiteral, Dot, CapitalizedIdentifier]);
    assert_eq!(
        kinds("1..3"),
        [IntegerLiteral, RangeInclusive, IntegerLiteral]
    );
    assert_eq!(
        kinds("1..<3"),
        [IntegerLiteral, RangeExclusiveRight, IntegerLiteral]
    );
}

#[test]
fn comments_are_line_breaks() {
    assert_eq!(
        kinds("a // c\nb /* x */ c"),
        [Identifier, Nl, Nl, Identifier, Nl, Identifier]
    );
    assert_eq!(kinds("#!/usr/bin/env nextflow\nx"), [Nl, Nl, Identifier]);
    assert_eq!(kinds("a \\\n b"), [Identifier, Identifier]);
}

#[test]
fn negated_in_and_instanceof() {
    assert_eq!(kinds("a !in b"), [Identifier, NotIn, Identifier]);
    assert_eq!(kinds("!inputs"), [Not, Identifier]);
    assert_eq!(
        kinds("a !instanceof B"),
        [Identifier, NotInstanceof, CapitalizedIdentifier]
    );
}

#[test]
fn shifts_are_split_into_angle_brackets() {
    assert_eq!(kinds("a << b"), [Identifier, Lt, Lt, Identifier]);
    assert_eq!(kinds("a >> b"), [Identifier, Gt, Gt, Identifier]);
    assert_eq!(kinds("a >>= b"), [Identifier, RShiftAssign, Identifier]);
    assert_eq!(
        kinds("a ?: b ?. c"),
        [Identifier, Elvis, Identifier, SafeDot, Identifier]
    );
}

#[test]
fn keywords_depend_on_the_dialect() {
    assert_eq!(kinds("process foo {"), [Process, Identifier, LBrace]);
    assert_eq!(kinds("inputs"), [Identifier]);
    assert_eq!(kinds_in("process {", Dialect::Config), [Identifier, LBrace]);
    assert_eq!(
        kinds_in("includeConfig 'x'", Dialect::Config),
        [IncludeConfig, StringLiteral]
    );
    assert_eq!(
        kinds("true null int Foo"),
        [
            BooleanLiteral,
            NullLiteral,
            BuiltInPrimitiveType,
            CapitalizedIdentifier
        ]
    );
}

#[test]
fn lone_dollar_in_gstring_is_an_error() {
    let lexed = tokenize("\"a $ b\"", Dialect::Script);
    assert_eq!(lexed.errors.len(), 1);
}

#[test]
fn unterminated_strings_are_errors() {
    assert_eq!(tokenize("'abc\nx", Dialect::Script).errors.len(), 1);
    assert_eq!(tokenize("\"a $x\nb", Dialect::Script).errors.len(), 1);
}
