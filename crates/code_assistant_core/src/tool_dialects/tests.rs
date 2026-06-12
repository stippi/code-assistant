//! Cross-dialect tests: prompt documentation generation and mixed-syntax
//! scenarios.


#[test]
fn test_tool_use_docs_generation() {
    use crate::agent::ToolSyntax;
    use crate::tools::core::ToolScope;

    // Test XML documentation
    let xml_parser = crate::tool_dialects::dialect_for(ToolSyntax::Xml);
    if let Some(xml_docs) = xml_parser.render_tool_section_for_prompt(&crate::tools::test_registry(), ToolScope::Agent.tag()) {
        println!("=== XML Tool Documentation ===");
        println!("{}", &xml_docs[..1500.min(xml_docs.len())]);
        println!("...\n");
    }

    // Test Caret documentation
    let caret_parser = crate::tool_dialects::dialect_for(ToolSyntax::Caret);
    if let Some(caret_docs) = caret_parser.render_tool_section_for_prompt(&crate::tools::test_registry(), ToolScope::Agent.tag()) {
        println!("=== Caret Tool Documentation ===");
        println!("{}", &caret_docs[..1500.min(caret_docs.len())]);
        println!("...\n");
    }

    // Test Native documentation (should be None)
    let native_parser = crate::tool_dialects::dialect_for(ToolSyntax::Native);
    if native_parser
        .render_tool_section_for_prompt(&crate::tools::test_registry(), ToolScope::Agent.tag())
        .is_some()
    {
        println!("Native parser unexpectedly returned documentation");
    } else {
        println!("=== Native Tool Documentation ===");
        println!("None (as expected - uses API tool definitions)\n");
    }

    // Test syntax documentation
    println!("=== XML Syntax Documentation ===");
    if let Some(xml_syntax) = xml_parser.render_format_section_for_prompt() {
        println!("{}", &xml_syntax[..1500.min(xml_syntax.len())]);
        println!("...\n");
    }

    println!("=== Caret Syntax Documentation ===");
    if let Some(caret_syntax) = caret_parser.render_format_section_for_prompt() {
        println!("{}", &caret_syntax[..1500.min(caret_syntax.len())]);
        println!("...\n");
    }

    println!("=== Native Syntax Documentation ===");
    if native_parser.render_format_section_for_prompt().is_some() {
        println!("Native parser unexpectedly returned syntax documentation");
    } else {
        println!("None (as expected - uses native function calls)\n");
    }
}

#[test]
fn test_parameter_documentation_formatting() {
    use crate::agent::ToolSyntax;
    use serde_json::json;

    // Test parameter formatting with XML parser
    let xml_parser = crate::tool_dialects::dialect_for(ToolSyntax::Xml);

    // Test simple parameter documentation
    let _param = json!({
        "type": "string",
        "description": "Path to the file"
    });

    // The parser methods are private, so we test via full tool documentation generation
    // and check that our formatting logic works correctly by testing specific tools

    // Test parameter with required marking in description
    let _required_param = json!({
        "type": "string",
        "description": "Project name (required)"
    });

    // Test that array parameters work correctly by checking generated docs contain proper examples
    if let Some(docs) = xml_parser.render_tool_section_for_prompt(&crate::tools::test_registry(), crate::tools::core::ToolScope::Agent.tag())
    {
        // Should contain XML-style parameter examples
        assert!(
            docs.contains("<param:"),
            "XML docs should contain parameter examples"
        );
        assert!(
            docs.contains("</param:"),
            "XML docs should contain parameter closing tags"
        );

        // Should contain required parameter markers
        assert!(
            docs.contains("(required)"),
            "XML docs should mark required parameters"
        );

        // Should handle multiline parameters properly
        assert!(
            docs.contains("command_line"),
            "XML docs should include multiline parameter examples"
        );

        println!("✓ XML parameter documentation formatting verified");
    }

    // Test with Caret parser
    let caret_parser = crate::tool_dialects::dialect_for(ToolSyntax::Caret);
    if let Some(docs) =
        caret_parser.render_tool_section_for_prompt(&crate::tools::test_registry(), crate::tools::core::ToolScope::Agent.tag())
    {
        // Should contain caret-style parameter examples
        assert!(
            docs.contains("^^^"),
            "Caret docs should contain caret block examples"
        );
        assert!(
            docs.contains(": "),
            "Caret docs should contain key: value parameter examples"
        );

        // Should contain required parameter markers
        assert!(
            docs.contains("(required)"),
            "Caret docs should mark required parameters"
        );

        // Should handle array parameters properly
        assert!(
            docs.contains("["),
            "Caret docs should include array parameter examples"
        );
        assert!(
            docs.contains("]"),
            "Caret docs should include array parameter examples"
        );

        // Should handle multiline parameters properly
        assert!(
            docs.contains("---"),
            "Caret docs should include multiline parameter examples"
        );

        println!("✓ Caret parameter documentation formatting verified");
    }
}

#[test]
fn test_usage_example_generation() {
    use crate::agent::ToolSyntax;

    // Test both XML and Caret parsers generate proper usage examples
    let xml_parser = crate::tool_dialects::dialect_for(ToolSyntax::Xml);
    let caret_parser = crate::tool_dialects::dialect_for(ToolSyntax::Caret);

    if let Some(xml_docs) =
        xml_parser.render_tool_section_for_prompt(&crate::tools::test_registry(), crate::tools::core::ToolScope::Agent.tag())
    {
        // XML usage examples should be properly formatted
        assert!(
            xml_docs.contains("<tool:"),
            "XML docs should contain tool examples"
        );
        assert!(
            xml_docs.contains("</tool:"),
            "XML docs should contain tool closing tags"
        );

        // Should handle array parameters correctly (paths -> path singular form)
        if xml_docs.contains("paths") {
            assert!(
                xml_docs.contains("<param:path>"),
                "XML docs should use singular form for array parameters"
            );
        }

        // Should contain realistic parameter placeholders
        assert!(
            xml_docs.contains("project-name") || xml_docs.contains("File path here"),
            "XML docs should contain realistic placeholders"
        );

        println!("✓ XML usage example generation verified");
    }

    if let Some(caret_docs) =
        caret_parser.render_tool_section_for_prompt(&crate::tools::test_registry(), crate::tools::core::ToolScope::Agent.tag())
    {
        // Caret usage examples should be properly formatted
        assert!(
            caret_docs.contains("^^^"),
            "Caret docs should contain caret block examples"
        );

        // Should handle multiline parameters correctly
        if caret_docs.contains("command_line") {
            assert!(
                caret_docs.contains("command_line ---"),
                "Caret docs should show multiline parameter syntax"
            );
            assert!(
                caret_docs.contains("--- command_line"),
                "Caret docs should show multiline parameter closing"
            );
        }

        // Should handle array parameters correctly
        if caret_docs.contains("paths") {
            assert!(
                caret_docs.contains("paths: ["),
                "Caret docs should show array parameter syntax"
            );
        }

        // Should contain realistic parameter placeholders
        assert!(
            caret_docs.contains("project-name") || caret_docs.contains("File path here"),
            "Caret docs should contain realistic placeholders"
        );

        println!("✓ Caret usage example generation verified");
    }
}

#[tokio::test]
async fn test_mixed_syntax_scenarios_with_filters() {
    use crate::tool_dialects::caret::parse_caret_tool_invocations;
    use crate::tool_dialects::xml::parse_xml_tool_invocations;
    use crate::tools::tool_use_filter::{SingleToolFilter, SmartToolFilter};

    // Test that each parser handles its own syntax correctly with filters

    // XML with web tools (read operations)
    let xml_text = r#"Let me fetch some web content:

<tool:web_fetch>
<param:url>https://example.com</param:url>
</tool:web_fetch>

<tool:web_search>
<param:query>rust programming</param:query>
<param:hits_page_number>1</param:hits_page_number>
</tool:web_search>"#;

    let registry = crate::tools::test_registry();
    let smart_filter = SmartToolFilter::new(&registry);
    let (xml_tools, _) = parse_xml_tool_invocations(
        xml_text,
        123,
        0,
        Some(&smart_filter),
        &crate::tools::test_registry(),
    )
    .unwrap();

    // Should get both web tools (they're read operations)
    assert_eq!(xml_tools.len(), 2);
    assert_eq!(xml_tools[0].name, "web_fetch");
    assert_eq!(xml_tools[1].name, "web_search");

    // Caret with the same tools
    let caret_text = concat!(
        "Let me fetch some web content:\n\n",
        "^^^web_fetch\n",
        "url: https://example.com\n",
        "^^^\n\n",
        "^^^web_search\n",
        "query: rust programming\n",
        "hits_page_number: 1\n",
        "^^^"
    );

    let (caret_tools, _) = parse_caret_tool_invocations(
        caret_text,
        123,
        0,
        Some(&smart_filter),
        &crate::tools::test_registry(),
    )
    .unwrap();

    // Should get both web tools (they're read operations)
    assert_eq!(caret_tools.len(), 2);
    assert_eq!(caret_tools[0].name, "web_fetch");
    assert_eq!(caret_tools[1].name, "web_search");

    // Test with SingleToolFilter - should only get first tool in both cases
    let single_filter = SingleToolFilter;

    let (xml_single, _) = parse_xml_tool_invocations(
        xml_text,
        123,
        0,
        Some(&single_filter),
        &crate::tools::test_registry(),
    )
    .unwrap();
    assert_eq!(xml_single.len(), 1);
    assert_eq!(xml_single[0].name, "web_fetch");

    let (caret_single, _) = parse_caret_tool_invocations(
        caret_text,
        123,
        0,
        Some(&single_filter),
        &crate::tools::test_registry(),
    )
    .unwrap();
    assert_eq!(caret_single.len(), 1);
    assert_eq!(caret_single[0].name, "web_fetch");
}
