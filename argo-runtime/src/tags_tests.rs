//! Tests for tag substitution.
// Copyright (c) 2026 The uwe Contributors
// SPDX-License-Identifier: MIT or Apache-2.0

use super::{Variables, has_tag, replace, replace_node};
use argo_parser::parse_yaml;

fn variables() -> Variables {
    Variables::new(
        [
            ("inputs.parameters.message", "hello"),
            ("inputs.parameters.file", "/data/a.bam"),
            ("workflow.name", "wf-abc"),
            ("item", "{\"k\":\"v\"}"),
            ("item.k", "v"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect(),
    )
}

#[test]
fn replaces_simple_tags() {
    let variables = variables();
    assert_eq!(
        replace(
            "say {{inputs.parameters.message}} in {{ workflow.name }}",
            &variables,
            false
        ),
        Ok("say hello in wf-abc".to_owned())
    );
    assert_eq!(
        replace("{{steps.a.outputs.result}}", &variables, true),
        Ok("{{steps.a.outputs.result}}".to_owned())
    );
    assert!(replace("{{steps.a.outputs.result}}", &variables, false).is_err());
    assert_eq!(
        replace("a {{ b", &variables, false),
        Ok("a {{ b".to_owned())
    );
    assert_eq!(replace("{{item.k}}", &variables, false), Ok("v".to_owned()));
}

#[test]
fn replaces_expression_tags() {
    let variables = variables();
    assert_eq!(
        replace(
            "{{=sprig.osBase(inputs.parameters.file)}}",
            &variables,
            false
        ),
        Ok("a.bam".to_owned())
    );
    // `item` is dropped where `item.k` continues it, so expressions see a map.
    assert_eq!(
        replace("{{=item.k}}", &variables, false),
        Ok("v".to_owned())
    );
    assert_eq!(
        replace("{{=tasks.a.outputs.result}}", &variables, true),
        Ok("{{=tasks.a.outputs.result}}".to_owned())
    );
    assert_eq!(
        replace("{{=sprig.int(retries)}}", &variables, true),
        Ok("{{=sprig.int(retries)}}".to_owned())
    );
    assert!(replace("{{=nothing}}", &variables, false).is_err());
    // Results other than strings are JSON, as Argo v4 writes them.
    assert_eq!(
        replace(
            "{{=[1, 2.5]}} {{= {'a': 'x y'} }} {{=3 / 2}} {{=true}}",
            &variables,
            false
        ),
        Ok(r#"[1,2.5] {"a":"x y"} 1.5 true"#.to_owned())
    );
    let without_item = Variables::new(
        [("inputs.parameters.n".to_owned(), "2".to_owned())]
            .into_iter()
            .collect(),
    );
    assert_eq!(
        replace(
            "{{=asInt(inputs.parameters.n) + item}}",
            &without_item,
            true
        ),
        Ok("{{=asInt(inputs.parameters.n) + item}}".to_owned())
    );
    let with_item =
        without_item.with_values([("item".to_owned(), "3".to_owned())].into_iter().collect());
    assert_eq!(
        replace(
            "{{=asInt(inputs.parameters.n) + asInt(item)}}",
            &with_item,
            false
        ),
        Ok("5".to_owned())
    );
}

#[test]
fn replaces_trees() {
    let mut node = parse_yaml("args: ['{{inputs.parameters.message}}']\n'{{workflow.name}}': 1\n")
        .expect("YAML")
        .expect("a document");
    replace_node(&mut node, &variables(), true).expect("replaces");
    let args = node.get("args").expect("args");
    let argo_parser::Value::Sequence(items) = &args.value else {
        panic!("a list");
    };
    assert_eq!(items[0].as_str(), Some("hello"));
    assert!(node.get("wf-abc").is_some());
    assert!(has_tag("x {{a}}"));
    assert!(!has_tag("x {{a"));
}
