//! Several lookups in one request: an operation repeated under one alias
//! per item. GitHub has no bulk form of the reads and mutations batched
//! here, but answers a document with many aliased fields in one go.
//!
//! Item `i` gets variables `$name_i` and its top-level fields the alias
//! `a{i}_field`. The answer is split back per item, and so are errors, by
//! the alias their path starts with: a missing pull request fails only its
//! own item.

use super::lexer::{fragment_spreads, is_name_char, walk};
use crate::error::{GraphQlError, PathSegment};
use serde_json::{Map, Value};

/// The operation repeated for `count` items. The operation must not use
/// named fragments (their variables could not be renamed per item).
pub(crate) fn aliased_operation(operation: &str, count: usize) -> String {
    assert!(
        fragment_spreads(operation).is_empty(),
        "aliased operations cannot use named fragments"
    );
    let header_end = operation.find('{').expect("operation has a body");
    let header = operation[..header_end].trim_end();
    let body = operation[header_end..].trim();
    let body = &body[1..body.len() - 1];
    let (head, variables) = match header.split_once('(') {
        Some((head, rest)) => (head.trim_end(), rest.trim_end().trim_end_matches(')')),
        None => (header, ""),
    };
    let variables: Vec<String> = (0..count)
        .filter(|_| !variables.trim().is_empty())
        .map(|i| rename_variables(variables, i))
        .collect();
    let bodies: Vec<String> = (0..count)
        .map(|i| {
            alias_fields(&rename_variables(body, i), i)
                .trim()
                .to_string()
        })
        .collect();
    if variables.is_empty() {
        format!("{head} {{\n{}\n}}", bodies.join("\n"))
    } else {
        format!(
            "{head}({}) {{\n{}\n}}",
            variables.join(", "),
            bodies.join("\n")
        )
    }
}

/// The variables of all items, each name suffixed with its item's index.
pub(crate) fn aliased_variables(items: &[Value]) -> Value {
    let mut all = Map::new();
    for (i, item) in items.iter().enumerate() {
        if let Value::Object(variables) = item {
            for (name, value) in variables {
                all.insert(format!("{name}_{i}"), value.clone());
            }
        }
    }
    Value::Object(all)
}

/// One item's share of the answer.
#[derive(Debug, PartialEq)]
pub(crate) struct ItemAnswer {
    /// The item's fields under their own names.
    pub data: Value,
    /// Errors whose path starts at one of the item's fields.
    pub errors: Vec<GraphQlError>,
}

/// Splits an aliased answer into `count` items. Errors without a path into
/// an item are returned separately: they concern the whole request.
pub(crate) fn split_answer(
    data: Option<Value>,
    errors: Vec<GraphQlError>,
    count: usize,
) -> (Vec<ItemAnswer>, Vec<GraphQlError>) {
    let mut items: Vec<ItemAnswer> = (0..count)
        .map(|_| ItemAnswer {
            data: Value::Object(Map::new()),
            errors: Vec::new(),
        })
        .collect();
    if let Some(Value::Object(fields)) = data {
        for (key, value) in fields {
            if let Some((index, field)) = parse_alias(&key)
                && let Some(Value::Object(item)) = items.get_mut(index).map(|i| &mut i.data)
            {
                item.insert(field.to_string(), value);
            }
        }
    }
    let mut general = Vec::new();
    for mut error in errors {
        let index = match error.path.first() {
            Some(PathSegment::Field(key)) => {
                parse_alias(key).map(|(index, field)| (index, field.to_string()))
            }
            _ => None,
        };
        match index {
            Some((index, field)) if index < count => {
                error.path[0] = PathSegment::Field(field);
                items[index].errors.push(error);
            }
            _ => general.push(error),
        }
    }
    (items, general)
}

fn parse_alias(key: &str) -> Option<(usize, &str)> {
    let rest = key.strip_prefix('a')?;
    let (index, field) = rest.split_once('_')?;
    Some((index.parse().ok()?, field))
}

fn rename_variables(text: &str, index: usize) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut in_variable = false;
    let mut last = 0;
    walk(text, |at, c, _| {
        if in_variable && !is_name_char(c) {
            out.push_str(&text[last..at]);
            out.push_str(&format!("_{index}"));
            last = at;
            in_variable = false;
        }
        if c == '$' {
            in_variable = true;
        }
    });
    out.push_str(&text[last..]);
    if in_variable {
        out.push_str(&format!("_{index}"));
    }
    out
}

/// Prefixes each top-level field (or its alias) with `a{index}_`.
fn alias_fields(body: &str, index: usize) -> String {
    let mut insert_at = Vec::new();
    let mut parens = 0usize;
    let mut previous = ' ';
    let mut in_name = false;
    walk(body, |at, c, depth| {
        match c {
            '(' => parens += 1,
            ')' => parens = parens.saturating_sub(1),
            _ => {}
        }
        let starts_name = is_name_char(c) && !in_name;
        in_name = is_name_char(c);
        if depth == 0 && parens == 0 && starts_name && !matches!(previous, '@' | ':' | '$') {
            insert_at.push(at);
        }
        if !c.is_whitespace() {
            previous = c;
        }
    });
    let mut out = String::with_capacity(body.len() + insert_at.len() * 8);
    let mut last = 0;
    for at in insert_at {
        out.push_str(&body[last..at]);
        let name: String = body[at..]
            .chars()
            .take_while(|c| is_name_char(*c))
            .collect();
        let is_alias = body[at + name.len()..].trim_start().starts_with(':');
        if is_alias {
            out.push_str(&format!("a{index}_"));
        } else {
            out.push_str(&format!("a{index}_{name}: "));
        }
        last = at;
    }
    out.push_str(&body[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repeats_an_operation_per_item_with_its_own_variables_and_aliases() {
        let operation = "query Stats($owner: String!, $number: Int!) {\n  repository(owner: $owner) {\n    pullRequest(number: $number) { additions }\n  }\n}";
        assert_eq!(
            aliased_operation(operation, 2),
            "query Stats($owner_0: String!, $number_0: Int!, $owner_1: String!, $number_1: Int!) {\n\
             a0_repository: repository(owner: $owner_0) {\n    pullRequest(number: $number_0) { additions }\n  }\n\
             a1_repository: repository(owner: $owner_1) {\n    pullRequest(number: $number_1) { additions }\n  }\n}"
        );
    }

    #[test]
    fn aliases_every_top_level_field_and_renames_variables_in_directives() {
        let operation = "mutation Set($id: ID!, $viewed: Boolean!) {\n  mark(input: { id: $id }) @include(if: $viewed) { x }\n  unmark(input: { id: $id }) @skip(if: $viewed) { x }\n}";
        let aliased = aliased_operation(operation, 1);
        assert!(
            aliased.contains("a0_mark: mark(input: { id: $id_0 }) @include(if: $viewed_0) { x }")
        );
        assert!(
            aliased.contains("a0_unmark: unmark(input: { id: $id_0 }) @skip(if: $viewed_0) { x }")
        );
    }

    #[test]
    fn suffixes_variables_per_item() {
        let variables =
            aliased_variables(&[json!({"owner": "a", "n": 1}), json!({"owner": "b", "n": 2})]);
        assert_eq!(
            variables,
            json!({"owner_0": "a", "n_0": 1, "owner_1": "b", "n_1": 2})
        );
    }

    #[test]
    fn splits_data_and_errors_back_per_item() {
        let data = json!({
            "a0_repository": {"pullRequest": {"additions": 3}},
            "a1_repository": {"pullRequest": null},
        });
        let errors = vec![
            GraphQlError {
                message: "Could not resolve to a PullRequest".into(),
                kind: Some("NOT_FOUND".into()),
                path: vec![
                    PathSegment::Field("a1_repository".into()),
                    PathSegment::Field("pullRequest".into()),
                ],
            },
            GraphQlError {
                message: "Something broke".into(),
                kind: None,
                path: vec![],
            },
        ];
        let (items, general) = split_answer(Some(data), errors, 2);
        assert_eq!(
            items[0].data,
            json!({"repository": {"pullRequest": {"additions": 3}}})
        );
        assert!(items[0].errors.is_empty());
        assert_eq!(items[1].errors.len(), 1);
        assert_eq!(
            items[1].errors[0].path[0],
            PathSegment::Field("repository".into())
        );
        assert_eq!(general.len(), 1);
    }
}
