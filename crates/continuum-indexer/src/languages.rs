//! Supported languages and their tree-sitter grammars.

use tree_sitter::Language;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Php,
    Lua,
}

impl Lang {
    /// Map a file extension to a language, or `None` if unsupported.
    pub fn from_extension(ext: &str) -> Option<Lang> {
        match ext {
            "rs" => Some(Lang::Rust),
            "py" | "pyi" => Some(Lang::Python),
            "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
            "ts" | "tsx" | "mts" | "cts" => Some(Lang::TypeScript),
            "go" => Some(Lang::Go),
            "php" | "phtml" => Some(Lang::Php),
            "lua" => Some(Lang::Lua),
            _ => None,
        }
    }

    pub fn slug(&self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::TypeScript => "typescript",
            Lang::Go => "go",
            Lang::Php => "php",
            Lang::Lua => "lua",
        }
    }

    /// The tree-sitter grammar for this language.
    pub fn tree_sitter(&self) -> Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
            // `LANGUAGE_PHP`, not `LANGUAGE_PHP_ONLY`: real `.php` files
            // interleave HTML around the code.
            Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            Lang::Lua => tree_sitter_lua::LANGUAGE.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_mapping() {
        assert_eq!(Lang::from_extension("rs"), Some(Lang::Rust));
        assert_eq!(Lang::from_extension("py"), Some(Lang::Python));
        assert_eq!(Lang::from_extension("jsx"), Some(Lang::JavaScript));
        assert_eq!(Lang::from_extension("tsx"), Some(Lang::TypeScript));
        assert_eq!(Lang::from_extension("go"), Some(Lang::Go));
        assert_eq!(Lang::from_extension("php"), Some(Lang::Php));
        assert_eq!(Lang::from_extension("lua"), Some(Lang::Lua));
        assert_eq!(Lang::from_extension("txt"), None);
        assert_eq!(Lang::from_extension(""), None);
    }

    #[test]
    fn every_grammar_loads() {
        for lang in [
            Lang::Rust,
            Lang::Python,
            Lang::JavaScript,
            Lang::TypeScript,
            Lang::Go,
            Lang::Php,
            Lang::Lua,
        ] {
            let mut parser = tree_sitter::Parser::new();
            assert!(
                parser.set_language(&lang.tree_sitter()).is_ok(),
                "{lang:?} grammar"
            );
        }
    }
}
