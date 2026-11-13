//! Checks regular expressions against Java's `String` methods (Groovy spec, section 8.6).
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use nextflow_evaluator::regex::Pattern;

fn pattern(source: &str) -> Pattern {
    Pattern::new(source).expect("valid pattern")
}

#[test]
fn find_and_match() {
    assert!(pattern("b").find("abc").unwrap());
    assert!(!pattern("b").matches("abc").unwrap());
    assert!(pattern("a.c").matches("abc").unwrap());
    assert!(pattern("(?i)abc").matches("ABC").unwrap());
    assert!(pattern(r"\d+").matches("123").unwrap());
    assert!(!pattern("a|b").matches("ab").unwrap());
    assert!(Pattern::new("(").is_err());
}

#[test]
fn split_like_java() {
    let split = |text: &str, regex: &str, limit| pattern(regex).split(text, limit).unwrap();
    assert_eq!(split("x_y_z", "_", 0), ["x", "y", "z"]);
    assert_eq!(split("a.b.c", ".", 0), Vec::<String>::new());
    assert_eq!(split("a,b,,", ",", 0), ["a", "b"]);
    assert_eq!(split("a,b,,", ",", -1), ["a", "b", "", ""]);
    assert_eq!(split("", ",", 0), [""]);
    assert_eq!(split(",a", ",", 0), ["", "a"]);
    assert_eq!(split("abc", "", 0), ["a", "b", "c"]);
    assert_eq!(split("a,b,c", ",", 2), ["a", "b,c"]);
    assert_eq!(split("a1b22c", r"\d+", 0), ["a", "b", "c"]);
}

#[test]
fn replace_like_java() {
    let replace = |text: &str, regex: &str, with: &str| pattern(regex).replace(text, with, true);
    assert_eq!(
        replace("sample_R1.fq", r"_R(\d)", "-$1").unwrap(),
        "sample-1.fq"
    );
    assert_eq!(replace("aaa", "a", "b").unwrap(), "bbb");
    assert_eq!(replace("a.b", r"\.", r"\$").unwrap(), "a$b");
    assert_eq!(replace("ab", "(?<x>a)", "${x}${x}").unwrap(), "aab");
    assert_eq!(replace("ab", "(a)", "$12").unwrap(), "a2b");
    assert_eq!(pattern("a").replace("aaa", "b", false).unwrap(), "baa");
    assert!(replace("ab", "a", "$").is_err());
    assert!(replace("ab", "a", "$2").is_err());
}

#[test]
fn runtime_errors_propagate() {
    let regex = pattern(r"(?i)(a|b|ab)*(?>c)");
    let text = "ab".repeat(80);
    assert!(regex.find(&text).is_err());
    assert!(regex.matches(&text).is_err());
    assert!(regex.all_groups(&text).is_err());
    assert!(regex.split(&text, 0).is_err());
    assert!(regex.replace(&text, "x", true).is_err());
    assert!(
        regex
            .replace_with(&text, true, |error| error, |_| Ok(String::new()))
            .is_err()
    );
}

#[test]
fn java_character_classes() {
    for (source, input) in [(r"\d", "١"), (r"\w", "é"), (r"\s", "\u{a0}")] {
        assert!(!pattern(source).matches(input).unwrap());
        assert!(pattern(&format!("(?U){source}")).matches(input).unwrap());
        assert!(!pattern(&format!("(?u){source}")).matches(input).unwrap());
    }
    assert!(pattern(r"(?U:\w)(?-U:\w)").matches("éa").unwrap());
    assert!(!pattern(r"(?U:\w)\w").matches("éé").unwrap());
    assert!(pattern(r"[\D]").matches("١").unwrap());
    assert!(pattern(r"\Q\d\E").matches(r"\d").unwrap());
}

#[test]
fn java_case_flags() {
    for source in ["(?i)é", "(?i)[é]", r"(?i)\u00e9"] {
        assert!(!pattern(source).matches("É").unwrap(), "{source}");
        assert!(pattern(source).matches("é").unwrap(), "{source}");
    }
    for source in ["(?iu)é", "(?iU)é", "(?i)(?u)é", "(?u)(?i)é"] {
        assert!(pattern(source).matches("É").unwrap(), "{source}");
    }
    assert!(pattern("(?i)[a-z]+").matches("ABC").unwrap());
    assert!(!pattern("(?i)[a-z]+").matches("K").unwrap());
    assert!(!pattern("(?i)[^a]").matches("A").unwrap());
    assert!(pattern("(?i)[a-z&&[^c]]").matches("A").unwrap());
    assert!(!pattern("(?i)[a-z&&[^c]]").matches("C").unwrap());
    assert!(pattern("(?i:é)(?iu:é)").matches("éÉ").unwrap());
    assert!(!pattern("(?iu:é)(?i:é)").matches("ÉÉ").unwrap());
    assert!(pattern(r"(?i)(?<name>a)\k<name>").matches("aA").unwrap());
    assert!(pattern(r"(?i)(a)\1").matches("aA").unwrap());
    assert!(pattern(r"(?i)(a)\1]").matches("aA]").unwrap());
    assert!(pattern(r"(?iu)(é)\1").matches("éÉ").unwrap());
    assert!(!pattern(r"(?iu-u)é").matches("É").unwrap());
    assert!(!pattern(r"(?iU-U)é").matches("É").unwrap());
    for source in [r"(?iu)\w", r"(?iu)[\w]"] {
        assert!(!pattern(source).matches("K").unwrap(), "{source}");
    }
    assert!(pattern(r"(?iu)\W").matches("K").unwrap());
    assert!(pattern(r"(?iu)[k]").matches("K").unwrap());
    assert!(pattern(r"(?iU)\w").matches("K").unwrap());
    for source in [r"(?iu)i", r"(?iu)[i]", r"(?iu)\u0130"] {
        for text in ["i", "I", "ı", "İ"] {
            assert!(pattern(source).matches(text).unwrap(), "{source}: {text}");
        }
    }
    assert!(!pattern(r"(?iu)[^i]").matches("İ").unwrap());
}

#[test]
#[ignore = "fancy-regex 0.19.2 backrefs share global Unicode mode and consume the captured byte length"]
fn java_backref_flags() {
    assert!(!pattern(r"(?i)(é)\1").matches("éÉ").unwrap());
    assert!(pattern(r"(?iu)(k)\1").matches("kK").unwrap());
    assert!(pattern(r"(?iu)(i)\1").matches("iı").unwrap());
}

#[test]
fn matcher_cursor() {
    use nextflow_evaluator::regex::Matcher;
    use std::rc::Rc;
    let matcher = Matcher::new(Rc::new(pattern("a")), "a".repeat(100000)).unwrap();
    assert!(matcher.find().unwrap());
    assert_eq!(
        matcher.current.borrow().as_ref().unwrap()[0].as_deref(),
        Some("a")
    );
    let empty = Matcher::new(Rc::new(pattern("")), "é".into()).unwrap();
    assert!(empty.find().unwrap());
    assert!(empty.find().unwrap());
    assert!(!empty.find().unwrap());
}
