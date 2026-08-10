//! Conda versions and their order, as conda's `VersionOrder` compares them, and the version
//! parts of match specs, like `>=1.2,<2`, `1.8.*` or `1.1|1.3`.
//!
//! A version has an epoch (`1!2.0`), a main part and a local part (`+local`). Each part splits
//! at `.` (and at `_`, except at the end) into components, and each component into runs of
//! digits and of other characters, lower-cased. Numbers compare as numbers; a text is smaller
//! than a number, `dev` is smaller than any other text, and `post` is larger than any number. A
//! component that starts with a text gets a 0 in front, and shorter lists count as padded with
//! 0, so `1.1a1 < 1.1 = 1.1.0 < 1.1.0post1`.
// Copyright (c) 2026 The iunctus Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use std::cmp::Ordering;
use std::fmt;

/// A part of a component of a version.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Atom {
    /// `dev`, smaller than any other text.
    Dev,
    Text(String),
    Number(u64),
    /// `post`, larger than any number.
    Post,
}

impl Atom {
    fn rank(&self) -> u8 {
        match self {
            Atom::Dev => 0,
            Atom::Text(_) => 1,
            Atom::Number(_) => 2,
            Atom::Post => 3,
        }
    }
}

impl Ord for Atom {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Atom::Number(a), Atom::Number(b)) => a.cmp(b),
            (Atom::Text(a), Atom::Text(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }
}

impl PartialOrd for Atom {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The padding of shorter lists: the number 0.
const PAD: Atom = Atom::Number(0);

type Components = Vec<Vec<Atom>>;

/// A conda version, like `1.21.6`, `2.30.0` or `1!1.0rc1+local`.
#[derive(Clone, Debug)]
pub struct Version {
    text: String,
    epoch: u64,
    main: Components,
    local: Components,
}

impl Version {
    /// Reads a version. A version with a space, a `*` or an operator is no version.
    pub fn parse(text: &str) -> Option<Version> {
        let lower = text.trim().to_ascii_lowercase();
        // Conda versions hold no `-`, which separates the parts of a package's file name.
        let valid = |c: char| c.is_ascii_alphanumeric() || "._+!".contains(c);
        if lower.is_empty() || !lower.chars().all(valid) {
            return None;
        }
        let (epoch, rest) = match lower.split_once('!') {
            Some((epoch, rest)) => (epoch.parse().ok()?, rest),
            None => (0, lower.as_str()),
        };
        let (main, local) = match rest.split_once('+') {
            Some((main, local)) => (main, local),
            None => (rest, ""),
        };
        if main.is_empty() {
            return None;
        }
        Some(Version {
            text: text.trim().to_owned(),
            epoch,
            main: components(main),
            local: components(local),
        })
    }

    /// Whether this version starts with the components of `prefix`, as `1.8.1` starts with
    /// `1.8`, but `1.80` does not.
    pub fn starts_with(&self, prefix: &Version) -> bool {
        self.epoch == prefix.epoch
            && prefix.main.len() <= self.main.len()
            && (prefix.main.iter().zip(&self.main))
                .enumerate()
                .all(|(index, (want, have))| {
                    // The last component of a prefix may stop inside a component: `1.1` of
                    // `1.1a1`.
                    match index + 1 == prefix.main.len() {
                        true => have.starts_with(want),
                        false => want == have,
                    }
                })
    }
}

/// Splits a version part into components and their atoms.
fn components(part: &str) -> Components {
    if part.is_empty() {
        return Vec::new();
    }
    // An underscore at the end stays, like openssl's `1.0.2_`; others separate.
    let (body, trailing) = match part.strip_suffix('_') {
        Some(body) => (body, true),
        None => (part, false),
    };
    let mut components: Components = body
        .split(['.', '_'])
        .map(|component| {
            let mut atoms = atoms(component);
            if !matches!(atoms.first(), Some(Atom::Number(_))) {
                atoms.insert(0, PAD);
            }
            atoms
        })
        .collect();
    if trailing && let Some(last) = components.last_mut() {
        last.push(Atom::Text("_".into()));
    }
    components
}

/// The runs of digits and of other characters of a component.
fn atoms(component: &str) -> Vec<Atom> {
    let mut atoms = Vec::new();
    let mut run = String::new();
    let mut digits = false;
    let push = |run: &mut String, digits: bool, atoms: &mut Vec<Atom>| {
        if run.is_empty() {
            return;
        }
        atoms.push(match (digits, run.as_str()) {
            (true, number) => Atom::Number(number.parse().unwrap_or(u64::MAX)),
            (false, "dev") => Atom::Dev,
            (false, "post") => Atom::Post,
            (false, text) => Atom::Text(text.to_owned()),
        });
        run.clear();
    };
    for c in component.chars() {
        if c.is_ascii_digit() != digits && !run.is_empty() {
            push(&mut run, digits, &mut atoms);
        }
        digits = c.is_ascii_digit();
        run.push(c);
    }
    push(&mut run, digits, &mut atoms);
    atoms
}

/// Compares two lists, the shorter one padded with 0.
fn compare_components(a: &Components, b: &Components) -> Ordering {
    let empty = Vec::new();
    for index in 0..a.len().max(b.len()) {
        let (x, y) = (
            a.get(index).unwrap_or(&empty),
            b.get(index).unwrap_or(&empty),
        );
        for position in 0..x.len().max(y.len()) {
            let (p, q) = (
                x.get(position).unwrap_or(&PAD),
                y.get(position).unwrap_or(&PAD),
            );
            match p.cmp(q) {
                Ordering::Equal => {}
                other => return other,
            }
        }
    }
    Ordering::Equal
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.epoch
            .cmp(&other.epoch)
            .then_with(|| compare_components(&self.main, &other.main))
            .then_with(|| compare_components(&self.local, &other.local))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Version {}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// The versions that a match spec allows: alternatives (`|`) of all of their constraints
/// (`,`).
#[derive(Clone, Debug, PartialEq)]
pub struct VersionSpec {
    any_of: Vec<Vec<Constraint>>,
}

/// One constraint on a version.
#[derive(Clone, Debug, PartialEq)]
enum Constraint {
    Any,
    Equal(Version),
    NotEqual(Version),
    Greater(Version),
    GreaterOrEqual(Version),
    Less(Version),
    LessOrEqual(Version),
    /// `1.8.*` or `1.8*`, and `=1.8`: versions that start with these components.
    StartsWith(Version),
    /// `~=1.8.1`: at least this version, and the same components but the last.
    Compatible(Version),
}

impl VersionSpec {
    /// Every version.
    pub fn any() -> VersionSpec {
        VersionSpec {
            any_of: vec![vec![Constraint::Any]],
        }
    }

    /// Reads the version part of a match spec. A plain version is exact when `exact` is set,
    /// as after `==` or in `name 1.8.1`, else a prefix, as in `name=1.8`.
    pub fn parse(text: &str, exact: bool) -> Option<VersionSpec> {
        let text = text.trim();
        if text.is_empty() || text == "*" {
            return Some(VersionSpec::any());
        }
        let any_of = (text.split('|'))
            .map(|all| {
                (all.split(','))
                    .map(|one| constraint(one.trim(), exact))
                    .collect::<Option<Vec<_>>>()
            })
            .collect::<Option<Vec<_>>>()?;
        Some(VersionSpec { any_of })
    }

    pub fn matches(&self, version: &Version) -> bool {
        (self.any_of.iter()).any(|all| all.iter().all(|one| one.matches(version)))
    }
}

/// One constraint, like `>=1.2`, `1.8.*` or `2.30.0`.
fn constraint(text: &str, exact: bool) -> Option<Constraint> {
    let version = |rest: &str| Version::parse(rest.trim());
    if text.is_empty() || text == "*" {
        return Some(Constraint::Any);
    }
    for (operator, make) in [
        (
            ">=",
            Constraint::GreaterOrEqual as fn(Version) -> Constraint,
        ),
        ("<=", Constraint::LessOrEqual),
        ("==", Constraint::Equal),
        ("!=", Constraint::NotEqual),
        ("~=", Constraint::Compatible),
        (">", Constraint::Greater),
        ("<", Constraint::Less),
    ] {
        if let Some(rest) = text.strip_prefix(operator) {
            // `==1.8.*` and `!=1.8.*` name prefixes too.
            if let Some(prefix) = rest
                .trim()
                .strip_suffix(".*")
                .or(rest.trim().strip_suffix('*'))
                && matches!(operator, "==")
            {
                return Some(Constraint::StartsWith(version(prefix)?));
            }
            return Some(make(version(rest)?));
        }
    }
    if let Some(rest) = text.strip_prefix('=') {
        let rest = rest.trim_end_matches('*').trim_end_matches('.');
        return Some(Constraint::StartsWith(version(rest)?));
    }
    if let Some(prefix) = text.strip_suffix(".*").or(text.strip_suffix('*')) {
        return Some(Constraint::StartsWith(version(
            prefix.trim_end_matches('.'),
        )?));
    }
    let version = version(text)?;
    Some(match exact {
        true => Constraint::Equal(version),
        false => Constraint::StartsWith(version),
    })
}

impl Constraint {
    fn matches(&self, have: &Version) -> bool {
        match self {
            Constraint::Any => true,
            Constraint::Equal(want) => have == want,
            Constraint::NotEqual(want) => have != want,
            Constraint::Greater(want) => have > want,
            Constraint::GreaterOrEqual(want) => have >= want,
            Constraint::Less(want) => have < want,
            Constraint::LessOrEqual(want) => have <= want,
            Constraint::StartsWith(want) => have.starts_with(want),
            Constraint::Compatible(want) => {
                let mut prefix = want.clone();
                if prefix.main.len() > 1 {
                    prefix.main.pop();
                }
                have >= want && have.starts_with(&prefix)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("a version: {text}"))
    }

    /// The order of the conda documentation (conda.models.version), where each version is at
    /// most the next one; `0.4.1.rc` and `0.4.1.RC` are equal, as case does not count.
    #[test]
    fn versions_follow_the_order_of_conda() {
        let ordered = [
            "0.4",
            "0.4.0",
            "0.4.1.rc",
            "0.4.1.RC",
            "0.4.1",
            "0.4.2",
            "0.5a1",
            "0.5b3",
            "0.5C1",
            "0.5",
            "0.9.6",
            "1.0",
            "1.1dev1",
            "1.1_",
            "1.1a1",
            "1.1.0dev1",
            "1.1.a1",
            "1.1.0rc1",
            "1.1.0",
            "1.1.0post1",
            "1.1post1",
            "1996.07.12",
            "1!0.4.1",
            "1!3.1.1.6",
            "2!0.4.1",
        ];
        for pair in ordered.windows(2) {
            assert!(
                version(pair[0]) <= version(pair[1]),
                "{} <= {}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(version("0.4"), version("0.4.0"));
        assert_eq!(version("0.4.1.rc"), version("0.4.1.RC"));
        assert!(version("0.5a1") < version("0.5"));
        assert!(version("1.1post1") > version("1.1.0"));
        assert!(version("1.10") > version("1.9"));
        assert!(Version::parse("1.2 3").is_none());
    }

    #[test]
    fn specs_allow_ranges_prefixes_and_alternatives() {
        let spec = |text: &str, exact| VersionSpec::parse(text, exact).expect("a spec");
        assert!(spec(">=1.2,<2", false).matches(&version("1.9.9")));
        assert!(!spec(">=1.2,<2", false).matches(&version("2.0")));
        assert!(spec("1.8.*", false).matches(&version("1.8.3")));
        assert!(!spec("1.8.*", false).matches(&version("1.80")));
        assert!(spec("1.8", false).matches(&version("1.8.1")));
        assert!(!spec("1.8", true).matches(&version("1.8.1")));
        assert!(spec("1.8", true).matches(&version("1.8.0")));
        assert!(spec("1.1|1.3", true).matches(&version("1.3")));
        assert!(spec("~=1.4.2", false).matches(&version("1.4.9")));
        assert!(!spec("~=1.4.2", false).matches(&version("1.5")));
        assert!(spec("!=1.4", false).matches(&version("1.5")));
        assert!(spec("==3.9.*", false).matches(&version("3.9.18")));
        assert!(spec("=3.9", false).matches(&version("3.9.18")));
        assert!(spec("*", true).matches(&version("0")));
        assert!(VersionSpec::parse(">=x y", false).is_none());
    }
}
