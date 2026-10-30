//! Just enough GraphQL syntax to cut documents into definitions, find
//! fragment spreads and rewrite operations for aliasing. Strings and
//! comments are respected; nothing is validated.

pub(crate) struct Definition {
    /// `query`, `mutation` or `fragment`.
    pub keyword: String,
    pub name: String,
    pub text: String,
}

pub(crate) fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The document without comments and blank lines.
pub(crate) fn strip_comments(document: &str) -> String {
    let mut out = String::with_capacity(document.len());
    for line in document.lines() {
        let mut kept = String::new();
        let mut in_string = false;
        let mut escaped = false;
        for c in line.chars() {
            if in_string {
                kept.push(c);
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '#' => break,
                '"' => in_string = true,
                _ => {}
            }
            kept.push(c);
        }
        let kept = kept.trim_end();
        if !kept.trim().is_empty() {
            out.push_str(kept);
            out.push('\n');
        }
    }
    out
}

/// Visits each character outside strings with the brace depth before it.
pub(crate) fn walk(text: &str, mut visit: impl FnMut(usize, char, usize)) {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (index, c) in text.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        visit(index, c, depth);
        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
}

/// The top-level definitions of a document.
pub(crate) fn definitions(document: &str) -> Vec<Definition> {
    let document = strip_comments(document);
    let mut definitions = Vec::new();
    let mut start: Option<usize> = None;
    walk(&document, |index, c, depth| {
        if depth == 0 && start.is_none() && !c.is_whitespace() {
            start = Some(index);
        }
        if depth == 1
            && c == '}'
            && let Some(begin) = start.take()
        {
            let text = document[begin..=index].trim().to_string();
            let mut words = text
                .split(|c: char| !is_name_char(c))
                .filter(|w| !w.is_empty());
            let keyword = words.next().unwrap_or_default().to_string();
            let name = words.next().unwrap_or_default().to_string();
            definitions.push(Definition {
                keyword,
                name,
                text,
            });
        }
    });
    definitions
}

/// Names of the fragments `...Name` spreads in `text` (not inline fragments).
pub(crate) fn fragment_spreads(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("...") {
        rest = &rest[at + 3..];
        let name: String = rest
            .trim_start()
            .chars()
            .take_while(|c| is_name_char(*c))
            .collect();
        if !name.is_empty() && name != "on" {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuts_a_document_into_definitions_without_comments() {
        let document = r#"
# A comment
query One($a: Int!) {
  x(arg: "a # not a comment") { y } # trailing
}

fragment F on T {
  z
}
"#;
        let definitions = definitions(document);
        assert_eq!(definitions.len(), 2);
        assert_eq!(definitions[0].keyword, "query");
        assert_eq!(definitions[0].name, "One");
        assert_eq!(
            definitions[0].text,
            "query One($a: Int!) {\n  x(arg: \"a # not a comment\") { y }\n}"
        );
        assert_eq!(definitions[1].keyword, "fragment");
        assert_eq!(definitions[1].name, "F");
    }

    #[test]
    fn finds_named_spreads_but_not_inline_fragments() {
        let text = "{ ...A ... on User { login } ... B }";
        assert_eq!(fragment_spreads(text), ["A", "B"]);
    }
}
