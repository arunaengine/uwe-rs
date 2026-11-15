//! Expands the macros of a tool as Galaxy does: imports, tokens, xml macros and their yields.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use crate::Error;
use crate::xml::{self, Element};

/// The macros of a tool and of the files it imports, in the order Galaxy loads them.
#[derive(Default)]
struct Library {
    tokens: Vec<(String, String)>,
    macros: Vec<(String, Element)>,
}

/// Expands the macros of the tool `root` in place. `load` reads an imported file by the path
/// its `<import>` gives, relative to the tool's folder.
pub fn expand(
    root: &mut Element,
    load: &mut dyn FnMut(&str) -> Result<String, Error>,
) -> Result<(), Error> {
    let Some(at) = root
        .children
        .iter()
        .position(|child| child.name == "macros")
    else {
        return Ok(());
    };
    let mut library = Library::default();
    let templates = collect(&root.children[at], load, &mut library)?;
    root.children[at] = Element {
        name: "macros".to_owned(),
        children: templates,
        line: root.children[at].line,
        ..Element::default()
    };
    let tokens = nested(library.tokens)?;
    let mut visited = Vec::new();
    while first_expand(root, &library.macros, &mut visited)? {}
    replace_tokens(root, &tokens);
    Ok(())
}

/// Adds the imports and then the own macros of `macros` to the library; gives the templates.
fn collect(
    macros: &Element,
    load: &mut dyn FnMut(&str) -> Result<String, Error>,
    library: &mut Library,
) -> Result<Vec<Element>, Error> {
    let mut templates = Vec::new();
    for import in macros.named("import") {
        let path = import.text.trim();
        let root = xml::read(&load(path)?, path)?;
        templates.extend(collect(&root, load, library)?);
    }
    let mut own: Vec<(&str, &Element)> = Vec::new();
    for element in macros.named("macro") {
        own.push((element.attribute("type").unwrap_or("xml"), element));
    }
    for kind in ["template", "xml", "token"] {
        own.extend(macros.named(kind).map(|element| (kind, element)));
    }
    for (kind, element) in own {
        let name = element.attribute("name").unwrap_or_default().to_owned();
        match kind {
            "token" => set(&mut library.tokens, name, element.text.clone()),
            "xml" => set(&mut library.macros, name, element.clone()),
            _ => templates.push(element.clone()),
        }
    }
    Ok(templates)
}

/// Sets a named entry; a later one replaces an earlier one at its first place, as a dict does.
fn set<T>(entries: &mut Vec<(String, T)>, name: String, value: T) {
    match entries.iter_mut().find(|(known, _)| *known == name) {
        Some(entry) => entry.1 = value,
        None => entries.push((name, value)),
    }
}

/// Replaces tokens inside the values of other tokens, in the order of Galaxy's loop.
fn nested(mut tokens: Vec<(String, String)>) -> Result<Vec<(String, String)>, Error> {
    for outer in 0..tokens.len() {
        let name = tokens[outer].0.clone();
        for inner in 0..tokens.len() {
            if !tokens[inner].1.contains(&name) {
                continue;
            }
            if inner == outer {
                let message = "a token cannot contain itself".to_owned();
                return Err(Error::Macro {
                    macro_name: name,
                    message,
                });
            }
            tokens[inner].1 = tokens[inner].1.replace(&name, &tokens[outer].1.clone());
        }
    }
    Ok(tokens)
}

/// Expands the first `<expand>` below `element` in document order; false when there is none.
fn first_expand(
    element: &mut Element,
    macros: &[(String, Element)],
    visited: &mut Vec<String>,
) -> Result<bool, Error> {
    for at in 0..element.children.len() {
        if element.children[at].name == "expand" {
            let expanded = expanded(&element.children[at], macros, visited)?;
            element.children.splice(at..=at, expanded);
            return Ok(true);
        }
        if first_expand(&mut element.children[at], macros, visited)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The elements that an `<expand>` stands for. Like lxml, the tail of the `<expand>` and the
/// text before the first child of the macro are dropped.
fn expanded(
    call: &Element,
    macros: &[(String, Element)],
    visited: &mut Vec<String>,
) -> Result<Vec<Element>, Error> {
    let name = call.attribute("macro").unwrap_or_default().to_owned();
    let failed = |message: String| Error::Macro {
        macro_name: name.clone(),
        message,
    };
    if visited.contains(&name) {
        return Err(failed(format!("the macros {visited:?} expand it again")));
    }
    let Some((_, definition)) = macros.iter().find(|(known, _)| *known == name) else {
        let known: Vec<&str> = macros.iter().map(|(known, _)| known.as_str()).collect();
        return Err(failed(format!(
            "no such macro; the known ones are {known:?}"
        )));
    };
    let mut body = definition.clone();
    for token in call.named("token") {
        let wanted = token.attribute("name").unwrap_or_default();
        let named = |element: &Element| element.attribute("name") == Some(wanted);
        if !replace_yields(&mut body, &named, &token.children) {
            return Err(failed(format!("no yield named {wanted}")));
        }
    }
    let rest: Vec<Element> = (call.children.iter())
        .filter(|child| child.name != "token")
        .cloned()
        .collect();
    replace_yields(&mut body, &|_| true, &rest);
    let parameters = parameters(definition, call).map_err(failed)?;
    if !parameters.is_empty() {
        body.children
            .iter_mut()
            .for_each(|child| replace_tokens(child, &parameters));
    }
    visited.push(name);
    while first_expand(&mut body, macros, visited)? {}
    visited.pop();
    Ok(body.children)
}

/// Replaces the `<yield>` elements below `element` that `wanted` picks; whether there was one.
fn replace_yields(
    element: &mut Element,
    wanted: &dyn Fn(&Element) -> bool,
    by: &[Element],
) -> bool {
    let mut found = false;
    let mut at = 0;
    while at < element.children.len() {
        if element.children[at].name == "yield" && wanted(&element.children[at]) {
            element.children.splice(at..=at, by.iter().cloned());
            at += by.len();
            found = true;
        } else {
            found |= replace_yields(&mut element.children[at], wanted, by);
            at += 1;
        }
    }
    found
}

/// The token parameters of a macro for one `<expand>`: `tokens="a,b"` are required, and
/// `token_x="..."` gives a default; the names become `@A@` with the quote character.
fn parameters(definition: &Element, call: &Element) -> Result<Vec<(String, String)>, String> {
    let mut quote = "@".to_owned();
    let mut declared: Vec<(String, Option<String>)> = Vec::new();
    for (key, value) in &definition.attributes {
        if key == "token_quote" {
            quote = value.clone();
        }
        if key == "tokens" {
            declared.extend(value.split(',').map(|token| (token.to_owned(), None)));
        } else if let Some(token) = key.strip_prefix("token_") {
            declared.push((token.to_owned(), Some(value.clone())));
        }
    }
    let mut parameters = Vec::new();
    for (key, default) in declared {
        let Some(value) = call.attribute(&key).map(str::to_owned).or(default) else {
            return Err(format!("the required parameter {key} is missing"));
        };
        set(
            &mut parameters,
            format!("{quote}{}{quote}", key.to_uppercase()),
            value,
        );
    }
    Ok(parameters)
}

/// Replaces tokens in the text and attributes of `element` and its descendants; like Galaxy,
/// it leaves tails as they are.
fn replace_tokens(element: &mut Element, tokens: &[(String, String)]) {
    element.text = replaced(&element.text, tokens);
    for (key, value) in &mut element.attributes {
        *key = replaced(key, tokens);
        *value = replaced(value, tokens);
    }
    for child in &mut element.children {
        replace_tokens(child, tokens);
    }
}

fn replaced(text: &str, tokens: &[(String, String)]) -> String {
    let mut text = text.to_owned();
    for (name, value) in tokens {
        if text.contains(name.as_str()) {
            text = text.replace(name.as_str(), value);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACROS: &str = r#"<macros>
    <token name="@VERSION@">1.@MINOR@</token>
    <token name="@MINOR@">2</token>
    <xml name="requirements">
        <requirements><requirement type="package" version="@VERSION@">tool</requirement><yield/></requirements>
    </xml>
    <xml name="option" tokens="value" token_label="Default label">
        <option value="@VALUE@">@LABEL@</option>
    </xml>
    <xml name="pair"><first><yield name="left"/></first><second><yield/></second></xml>
</macros>"#;

    fn expanded_tool(text: &str) -> Result<Element, Error> {
        let mut root = xml::read(text, "tool.xml")?;
        expand(&mut root, &mut |path| match path {
            "macros.xml" => Ok(MACROS.to_owned()),
            other => Err(Error::Import {
                path: other.to_owned(),
                source: std::io::Error::from(std::io::ErrorKind::NotFound),
            }),
        })?;
        Ok(root)
    }

    #[test]
    fn expands_tokens_yields() {
        let root = expanded_tool(
            r#"<tool version="@VERSION@+galaxy@SUFFIX@"><macros><import>macros.xml</import>
            <token name="@SUFFIX@">0</token></macros>
            <expand macro="requirements"><container type="docker">image</container></expand>
            <inputs><param name="p" type="select"><expand macro="option" value="a"/>
            <expand macro="option" value="b" label="B"/></param></inputs>
            <expand macro="pair"><token name="left"><l/></token><r/></expand></tool>"#,
        )
        .expect("expands");
        assert_eq!(root.attribute("version"), Some("1.2+galaxy0"));
        let requirements = root.child("requirements").expect("requirements");
        let requirement = requirements.child("requirement").expect("one package");
        assert_eq!(requirement.attribute("version"), Some("1.2"));
        assert!(
            requirements.child("container").is_some(),
            "the yield takes the body"
        );
        let options: Vec<(&str, &str)> = (root.child("inputs").and_then(|i| i.child("param")))
            .expect("a param")
            .named("option")
            .map(|option| {
                (
                    option.attribute("value").unwrap_or(""),
                    option.text.as_str(),
                )
            })
            .collect();
        assert_eq!(options, [("a", "Default label"), ("b", "B")]);
        assert!(root.child("first").and_then(|e| e.child("l")).is_some());
        assert!(root.child("second").and_then(|e| e.child("r")).is_some());
        assert!(root.child("macros").expect("kept").children.is_empty());
    }

    #[test]
    fn reports_missing_macros() {
        let unknown = expanded_tool("<tool><macros/><expand macro=\"nothing\"/></tool>");
        assert!(
            unknown
                .expect_err("unknown")
                .to_string()
                .contains("nothing")
        );
        let missing = expanded_tool(
            "<tool><macros><import>macros.xml</import></macros><expand macro=\"option\"/></tool>",
        );
        assert!(missing.expect_err("no value").to_string().contains("value"));
        let import = expanded_tool("<tool><macros><import>other.xml</import></macros></tool>");
        assert!(matches!(import, Err(Error::Import { .. })));
    }
}
