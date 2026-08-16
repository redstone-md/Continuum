//! Tree-sitter parsing: turn a source file into graph nodes.
//!
//! Rather than `.scm` query files, the indexer walks the syntax tree directly
//! and classifies nodes per language. This keeps grammar handling in plain Rust
//! and degrades gracefully -- an unknown node kind is simply skipped.

use continuum_graph::{CallSite, GraphNode, NodeKind};
use tree_sitter::{Node, Parser};

use crate::languages::Lang;

/// Result of parsing one file: its file node plus every symbol within it.
pub struct ParsedFile {
    pub file_node: GraphNode,
    pub symbols: Vec<GraphNode>,
}

struct RawSym {
    kind: NodeKind,
    name: String,
    start_byte: usize,
    end_byte: usize,
    start_line: usize,
    end_line: usize,
    signature: String,
    source: String,
    is_test: bool,
}

struct RawCall {
    name: String,
    line: usize,
    byte: usize,
}

/// Parse `source` for `rel_path`. Returns `None` if the grammar fails to load
/// or the file cannot be parsed at all.
pub fn parse(rel_path: &str, source: &str, lang: Lang) -> Option<ParsedFile> {
    let mut parser = Parser::new();
    if parser.set_language(&lang.tree_sitter()).is_err() {
        return None;
    }
    let tree = parser.parse(source, None)?;
    let src = source.as_bytes();

    let mut syms: Vec<RawSym> = Vec::new();
    let mut calls: Vec<RawCall> = Vec::new();
    walk(tree.root_node(), src, lang, 0, false, &mut syms, &mut calls);

    let mut nodes: Vec<GraphNode> = syms
        .iter()
        .map(|s| GraphNode {
            id: format!("{}::{}::{}", rel_path, s.name, s.start_line),
            kind: s.kind,
            name: s.name.clone(),
            path: rel_path.to_string(),
            language: String::new(),
            start_line: s.start_line,
            end_line: s.end_line,
            signature: s.signature.clone(),
            source: s.source.clone(),
            docstring: None,
            calls: Vec::new(),
            is_test: s.is_test,
        })
        .collect();

    // Attribute each call to the innermost enclosing symbol.
    for call in &calls {
        let mut best: Option<usize> = None;
        let mut best_span = usize::MAX;
        for (i, s) in syms.iter().enumerate() {
            if s.start_byte <= call.byte && call.byte < s.end_byte {
                let span = s.end_byte - s.start_byte;
                if span < best_span {
                    best_span = span;
                    best = Some(i);
                }
            }
        }
        if let Some(i) = best {
            nodes[i].calls.push(CallSite {
                name: call.name.clone(),
                line: call.line,
            });
        }
    }

    Some(ParsedFile {
        file_node: GraphNode::file(rel_path, lang.slug()),
        symbols: nodes,
    })
}

/// Maximum AST recursion depth. Real code nests far below this; the cap keeps a
/// pathologically nested file well within a worker thread's stack instead of
/// overflowing it.
const MAX_AST_DEPTH: usize = 512;

fn walk(
    node: Node,
    src: &[u8],
    lang: Lang,
    depth: usize,
    in_test: bool,
    syms: &mut Vec<RawSym>,
    calls: &mut Vec<RawCall>,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }
    // Crossing a Rust `#[cfg(test)]` module or `#[test]` function marks the
    // whole subtree as test code.
    let in_test = in_test
        || (lang == Lang::Rust
            && matches!(node.kind(), "mod_item" | "function_item")
            && rust_has_test_attr(node, src));
    if let Some((kind, name_field)) = def_spec(lang, node.kind()) {
        if let Some(name_node) = node.child_by_field_name(name_field) {
            if let Ok(name) = name_node.utf8_text(src).map(|n| trim_qualifier(lang, n)) {
                let source = text_of(node, src);
                let signature = source.lines().next().unwrap_or("").trim().to_string();
                syms.push(RawSym {
                    kind,
                    name: name.to_string(),
                    start_byte: node.start_byte(),
                    end_byte: node.end_byte(),
                    start_line: node.start_position().row + 1,
                    end_line: node.end_position().row + 1,
                    signature,
                    source,
                    is_test: in_test || name_is_test(lang, kind, name),
                });
            }
        }
    }
    if let Some(callee) = callee_name(lang, node, src) {
        calls.push(RawCall {
            name: callee,
            line: node.start_position().row + 1,
            byte: node.start_byte(),
        });
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.named_children(&mut cursor).collect();
    for child in children {
        walk(child, src, lang, depth + 1, in_test, syms, calls);
    }
}

/// True if a Rust item carries a `#[test]` or `#[cfg(test)]` attribute, found
/// among the `attribute_item` siblings that precede it.
fn rust_has_test_attr(node: Node, src: &[u8]) -> bool {
    let mut sibling = node.prev_named_sibling();
    while let Some(s) = sibling {
        match s.kind() {
            "attribute_item" => {
                if s.utf8_text(src).is_ok_and(|t| t.contains("test")) {
                    return true;
                }
                sibling = s.prev_named_sibling();
            }
            "line_comment" | "block_comment" => sibling = s.prev_named_sibling(),
            _ => return false,
        }
    }
    false
}

/// Lua declares functions under their full path (`function M.foo()`,
/// `function obj:method()`); index the final segment so call sites match.
fn trim_qualifier(lang: Lang, name: &str) -> &str {
    match lang {
        Lang::Lua => name.rsplit(['.', ':']).next().unwrap_or(name),
        _ => name,
    }
}

/// Per-language test-name conventions. Rust is covered by attributes instead.
fn name_is_test(lang: Lang, kind: NodeKind, name: &str) -> bool {
    match lang {
        Lang::Python => kind == NodeKind::Function && name.starts_with("test_"),
        Lang::Go => kind == NodeKind::Function && name.starts_with("Test") && name.len() > 4,
        // PHPUnit: `testFoo` methods on a `FooTest` class.
        Lang::Php => kind == NodeKind::Method && name.starts_with("test"),
        _ => false,
    }
}

/// Definition node kinds per language: `(symbol kind, name field name)`.
fn def_spec(lang: Lang, kind: &str) -> Option<(NodeKind, &'static str)> {
    match (lang, kind) {
        (Lang::Rust, "function_item") => Some((NodeKind::Function, "name")),
        (Lang::Rust, "struct_item") => Some((NodeKind::Struct, "name")),
        (Lang::Rust, "enum_item") => Some((NodeKind::Enum, "name")),
        (Lang::Rust, "trait_item") => Some((NodeKind::Trait, "name")),

        (Lang::Python, "function_definition") => Some((NodeKind::Function, "name")),
        (Lang::Python, "class_definition") => Some((NodeKind::Class, "name")),

        (Lang::JavaScript | Lang::TypeScript, "function_declaration") => {
            Some((NodeKind::Function, "name"))
        }
        (Lang::JavaScript | Lang::TypeScript, "generator_function_declaration") => {
            Some((NodeKind::Function, "name"))
        }
        (Lang::JavaScript | Lang::TypeScript, "class_declaration") => {
            Some((NodeKind::Class, "name"))
        }
        (Lang::JavaScript | Lang::TypeScript, "method_definition") => {
            Some((NodeKind::Method, "name"))
        }
        (Lang::TypeScript, "interface_declaration") => Some((NodeKind::Interface, "name")),

        (Lang::Go, "function_declaration") => Some((NodeKind::Function, "name")),
        (Lang::Go, "method_declaration") => Some((NodeKind::Method, "name")),
        (Lang::Go, "type_spec") => Some((NodeKind::Struct, "name")),

        (Lang::Php, "function_definition") => Some((NodeKind::Function, "name")),
        (Lang::Php, "method_declaration") => Some((NodeKind::Method, "name")),
        (Lang::Php, "class_declaration") => Some((NodeKind::Class, "name")),
        (Lang::Php, "interface_declaration") => Some((NodeKind::Interface, "name")),
        (Lang::Php, "trait_declaration") => Some((NodeKind::Trait, "name")),
        (Lang::Php, "enum_declaration") => Some((NodeKind::Enum, "name")),

        // Lua has no classes: metatable OOP is a runtime pattern the grammar
        // cannot see, so only named functions become symbols.
        (Lang::Lua, "function_declaration") => Some((NodeKind::Function, "name")),

        _ => None,
    }
}

/// Extract the callee name if `node` is a call expression.
fn callee_name(lang: Lang, node: Node, src: &[u8]) -> Option<String> {
    let kind = node.kind();
    let is_call = match lang {
        Lang::Rust => kind == "call_expression" || kind == "macro_invocation",
        Lang::Python => kind == "call",
        Lang::JavaScript | Lang::TypeScript | Lang::Go => kind == "call_expression",
        Lang::Php => matches!(
            kind,
            "function_call_expression"
                | "member_call_expression"
                | "nullsafe_member_call_expression"
                | "scoped_call_expression"
        ),
        Lang::Lua => kind == "function_call",
    };
    if !is_call {
        return None;
    }
    // The callee hides behind a different field per grammar.
    let field = match (lang, kind) {
        (Lang::Rust, "macro_invocation") => "macro",
        (Lang::Php, k) if k != "function_call_expression" => "name",
        (Lang::Lua, _) => "name",
        _ => "function",
    };
    name_from_callee(node.child_by_field_name(field)?, src)
}

fn name_from_callee(node: Node, src: &[u8]) -> Option<String> {
    let text = |n: Node| n.utf8_text(src).ok().map(str::to_string);
    match node.kind() {
        "identifier" | "field_identifier" | "property_identifier" | "type_identifier" | "name" => {
            text(node)
        }
        // PHP `\Foo\bar()` — the trailing segment is the callee.
        "qualified_name" => node
            .named_child(node.named_child_count().checked_sub(1)?)
            .and_then(text),
        "dot_index_expression" => node.child_by_field_name("field").and_then(text),
        "method_index_expression" => node.child_by_field_name("method").and_then(text),
        "field_expression" => node.child_by_field_name("field").and_then(text),
        "scoped_identifier" => node.child_by_field_name("name").and_then(text),
        "member_expression" => node.child_by_field_name("property").and_then(text),
        "attribute" => node.child_by_field_name("attribute").and_then(text),
        "selector_expression" => node.child_by_field_name("field").and_then(text),
        _ => None,
    }
}

fn text_of(node: Node, src: &[u8]) -> String {
    std::str::from_utf8(&src[node.start_byte()..node.end_byte()])
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(parsed: &ParsedFile) -> Vec<String> {
        parsed.symbols.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn parses_rust_functions_and_structs() {
        let src = "struct Point { x: i32 }\nfn add(a: i32) -> i32 { a + 1 }\n";
        let parsed = parse("p.rs", src, Lang::Rust).expect("parsed");
        let n = names(&parsed);
        assert!(n.contains(&"Point".to_string()));
        assert!(n.contains(&"add".to_string()));
    }

    #[test]
    fn attributes_calls_to_enclosing_function() {
        let src = "fn caller() { helper(); }\nfn helper() {}\n";
        let parsed = parse("p.rs", src, Lang::Rust).expect("parsed");
        let caller = parsed
            .symbols
            .iter()
            .find(|s| s.name == "caller")
            .expect("caller");
        assert!(caller.calls.iter().any(|c| c.name == "helper"));
    }

    #[test]
    fn signature_is_the_first_line() {
        let src = "fn wrapped(\n  a: i32,\n) {}\n";
        let parsed = parse("p.rs", src, Lang::Rust).expect("parsed");
        let f = parsed
            .symbols
            .iter()
            .find(|s| s.name == "wrapped")
            .expect("fn");
        assert_eq!(f.signature, "fn wrapped(");
    }

    #[test]
    fn parses_python_classes_and_methods() {
        let src = "class Animal:\n    def speak(self):\n        return 1\n";
        let parsed = parse("a.py", src, Lang::Python).expect("parsed");
        let n = names(&parsed);
        assert!(n.contains(&"Animal".to_string()));
        assert!(n.contains(&"speak".to_string()));
    }

    #[test]
    fn parses_javascript() {
        let src = "function greet(n) { return n; }\nclass Box {}\n";
        let parsed = parse("a.js", src, Lang::JavaScript).expect("parsed");
        let n = names(&parsed);
        assert!(n.contains(&"greet".to_string()));
        assert!(n.contains(&"Box".to_string()));
    }

    #[test]
    fn parses_go() {
        let src = "package main\nfunc Hello() string { return \"hi\" }\n";
        let parsed = parse("m.go", src, Lang::Go).expect("parsed");
        assert!(parsed.symbols.iter().any(|s| s.name == "Hello"));
    }

    #[test]
    fn parses_php_and_attributes_calls() {
        let src = "<?php\nclass Box {\n  function open() { helper(); $this->shut(); }\n}\nfunction helper() {}\n";
        let parsed = parse("a.php", src, Lang::Php).expect("parsed");
        let n = names(&parsed);
        assert!(n.contains(&"Box".to_string()));
        assert!(n.contains(&"helper".to_string()));
        let open = parsed
            .symbols
            .iter()
            .find(|s| s.name == "open")
            .expect("open");
        assert!(open.calls.iter().any(|c| c.name == "helper"));
        assert!(open.calls.iter().any(|c| c.name == "shut"));
    }

    #[test]
    fn php_test_methods_are_flagged() {
        let src = "<?php\nclass BoxTest {\n  function testOpens() {}\n}\n";
        let parsed = parse("t.php", src, Lang::Php).expect("parsed");
        let m = parsed
            .symbols
            .iter()
            .find(|s| s.name == "testOpens")
            .expect("method");
        assert!(m.is_test);
    }

    #[test]
    fn parses_lua_with_qualified_names() {
        let src =
            "local M = {}\nfunction M.greet(n) return n end\nfunction M.run() M.greet(1) end\n";
        let parsed = parse("a.lua", src, Lang::Lua).expect("parsed");
        let n = names(&parsed);
        assert!(n.contains(&"greet".to_string()), "got {n:?}");
        let run = parsed
            .symbols
            .iter()
            .find(|s| s.name == "run")
            .expect("run");
        assert!(run.calls.iter().any(|c| c.name == "greet"));
    }

    #[test]
    fn deeply_nested_input_does_not_overflow() {
        // A pathological nest must be bounded by MAX_AST_DEPTH, not crash.
        let src = format!("fn f() {{ {} }}", "if true {".repeat(9000)) + &"}".repeat(9000);
        let _ = parse("p.rs", &src, Lang::Rust);
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

        /// Parsing arbitrary input — including control characters and garbage —
        /// must never panic, for any language.
        #[test]
        fn parse_never_panics_on_arbitrary_input(src in ".{0,2000}") {
            for lang in [
                Lang::Rust,
                Lang::Python,
                Lang::JavaScript,
                Lang::TypeScript,
                Lang::Go,
                Lang::Php,
                Lang::Lua,
            ] {
                let _ = parse("fuzz", &src, lang);
            }
        }
    }
}
