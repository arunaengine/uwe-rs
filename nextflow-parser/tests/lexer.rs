//! Tests how the lexer splits Nextflow source text into tokens.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_parser::lexer::tokenize;
use nextflow_parser::token::{Dialect, TokenKind, TokenKind::*};

fn kinds_in(source: &str, dialect: Dialect) -> Vec<TokenKind> {
    let lexed = tokenize(source, dialect);
    assert!(
        lexed.errors.is_empty(),
        "unexpected errors: {:?}",
        lexed.errors
    );
    let mut kinds: Vec<_> = lexed.tokens.iter().map(|t| t.kind).collect();
    assert_eq!(kinds.pop(), Some(EndOfFile));
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
fn slash_starts_regex() {
    assert_eq!(kinds("a =~ /fo+/"), [Identifier, RegexFind, StringLiteral]);
    assert_eq!(
        kinds("a / b / c"),
        [Identifier, Divide, Identifier, Divide, Identifier]
    );
    assert_eq!(kinds("~/^a$/"), [BitNot, StringLiteral]);
    assert_eq!(
        kinds("(a) / 2"),
        [LeftParen, Identifier, RightParen, Divide, IntegerLiteral]
    );
}

#[test]
fn slashy_backslash_end() {
    assert_eq!(
        texts("x = /a\\\\/\ny"),
        ["x", "=", "/a\\\\/", "\n", "y", ""]
    );
    assert_eq!(texts("x = /a\\/b/"), ["x", "=", "/a\\/b/", ""]);
}

#[test]
fn string_kinds() {
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
fn nested_interpolation() {
    assert_eq!(
        kinds("\"${a.b { it }}\""),
        [
            GStringBegin { triple: false },
            InterpolationStart,
            Identifier,
            Dot,
            Identifier,
            LeftBrace,
            Identifier,
            RightBrace,
            RightBrace,
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
            InterpolationStart,
            Identifier,
            Dot,
            Identifier,
            RightBrace,
            GStringText,
            GStringEnd
        ]
    );
}

#[test]
fn triple_quote_runs() {
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
fn hex_fractions() {
    assert_eq!(texts("0x1.8p3 0x10.p-2"), ["0x1.8p3", "0x10.p-2", ""]);
    assert_eq!(texts("0x10.c"), ["0x10", ".", "c", ""]);
    assert_eq!(texts("0x1F.d(y)"), ["0x1F", ".", "d", "(", "y", ")", ""]);
}

#[test]
fn comments_become_newlines() {
    assert_eq!(
        kinds("a // c\nb /* x */ c"),
        [
            Identifier, Newline, Newline, Identifier, Newline, Identifier
        ]
    );
    assert_eq!(
        kinds("#!/usr/bin/env nextflow\nx"),
        [Newline, Newline, Identifier]
    );
    assert_eq!(kinds("a \\\n b"), [Identifier, Identifier]);
}

#[test]
fn negated_operators() {
    assert_eq!(kinds("a !in b"), [Identifier, NotIn, Identifier]);
    assert_eq!(kinds("!inputs"), [Not, Identifier]);
    assert_eq!(
        kinds("a !instanceof B"),
        [Identifier, NotInstanceof, CapitalizedIdentifier]
    );
}

#[test]
fn split_shift_operators() {
    assert_eq!(kinds("a << b"), [Identifier, Less, Less, Identifier]);
    assert_eq!(kinds("a >> b"), [Identifier, Greater, Greater, Identifier]);
    assert_eq!(kinds("a >>= b"), [Identifier, RightShiftAssign, Identifier]);
    assert_eq!(
        kinds("a ?: b ?. c"),
        [Identifier, Elvis, Identifier, SafeDot, Identifier]
    );
}

#[test]
fn dialect_keywords() {
    assert_eq!(kinds("process foo {"), [Process, Identifier, LeftBrace]);
    assert_eq!(kinds("inputs"), [Identifier]);
    assert_eq!(
        kinds_in("process {", Dialect::Config),
        [Identifier, LeftBrace]
    );
    assert_eq!(
        kinds_in("includeConfig 'x'", Dialect::Config),
        [IncludeConfig, StringLiteral]
    );
    assert_eq!(
        kinds("true null int Foo"),
        [
            BooleanLiteral,
            NullLiteral,
            PrimitiveType,
            CapitalizedIdentifier
        ]
    );
}

#[test]
fn rejects_lone_dollar() {
    let lexed = tokenize("\"a $ b\"", Dialect::Script);
    assert_eq!(lexed.errors.len(), 1);
}

#[test]
fn rejects_invalid_numbers() {
    let message = |source| {
        let errors = tokenize(source, Dialect::Script).errors;
        errors.first().map(|error| error.message.clone())
    };
    let underscore = "Number ending with underscores is invalid";
    assert_eq!(message("1_").as_deref(), Some(underscore));
    assert_eq!(message("1_.5").as_deref(), Some(underscore));
    assert_eq!(message("09").as_deref(), Some("Invalid octal number"));
    assert!(message("0x1.8p3").is_some_and(|m| m.starts_with("Character x is neither")));
    assert_eq!(message("1__0 0_7 08.5 0x1_F 0b1_0"), None);
    assert_eq!(texts("0x 0b2"), ["0", "x", "0", "b2", ""]);
}

#[test]
fn rejects_unterminated_strings() {
    assert_eq!(tokenize("'abc\nx", Dialect::Script).errors.len(), 1);
    assert_eq!(tokenize("\"a $x\nb", Dialect::Script).errors.len(), 1);
}
