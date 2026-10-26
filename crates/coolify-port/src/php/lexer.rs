//! A small, lossy PHP tokenizer.
//!
//! It is just precise enough for the structural questions the migration
//! engine asks of upstream source (route declarations, `use` imports,
//! Laravel schema-builder calls). It is not a PHP parser: unknown constructs
//! become [`Tok::Other`] and are skipped by callers.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    /// Identifier or qualified name, e.g. `Route`, `App\Models\User`.
    Ident(String),
    /// `$name`
    Var(String),
    /// Single- or double-quoted string, unescaped (no interpolation).
    Str(String),
    Num(String),
    /// Punctuation: `::`, `->`, `=>`, `(`, `)`, `[`, `]`, `{`, `}`, `,`, `;`, `?->` ...
    Punct(&'static str),
    Other(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub tok: Tok,
    /// 1-based source line.
    pub line: u32,
}

const PUNCTS: &[&str] = &[
    "?->", "::", "->", "=>", "(", ")", "[", "]", "{", "}", ",", ";", "=",
];

pub fn tokenize(src: &str) -> Vec<Token> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line = 1u32;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        // Opening tag.
        if starts_with(&chars, i, "<?php") {
            i += 5;
            continue;
        }
        // Comments.
        if starts_with(&chars, i, "//") || c == '#' && !starts_with(&chars, i, "#[") {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if starts_with(&chars, i, "/*") {
            i += 2;
            while i < chars.len() && !starts_with(&chars, i, "*/") {
                if chars[i] == '\n' {
                    line += 1;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        if c == '\'' || c == '"' {
            let start_line = line;
            let quote = c;
            let mut s = String::new();
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    let next = chars[i + 1];
                    // In single quotes only \' and \\ are escapes.
                    if quote == '\'' && next != '\'' && next != '\\' {
                        // Not an escape: keep the backslash literally.
                        s.push('\\');
                        i += 1;
                        continue;
                    } else {
                        s.push(match (quote, next) {
                            ('"', 'n') => '\n',
                            ('"', 't') => '\t',
                            _ => next,
                        });
                        i += 2;
                        continue;
                    }
                } else if chars[i] == '\n' {
                    line += 1;
                }
                s.push(chars[i]);
                i += 1;
            }
            i += 1;
            out.push(Token {
                tok: Tok::Str(s),
                line: start_line,
            });
            continue;
        }
        if c == '$' && i + 1 < chars.len() && is_ident_start(chars[i + 1]) {
            let start = i + 1;
            i += 1;
            while i < chars.len() && is_ident_char(chars[i]) {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Var(chars[start..i].iter().collect()),
                line,
            });
            continue;
        }
        if is_ident_start(c) || c == '\\' && i + 1 < chars.len() && is_ident_start(chars[i + 1]) {
            let start = i;
            i += 1;
            while i < chars.len()
                && (is_ident_char(chars[i])
                    || chars[i] == '\\' && i + 1 < chars.len() && is_ident_start(chars[i + 1]))
            {
                i += 1;
            }
            let ident: String = chars[start..i].iter().collect();
            out.push(Token {
                tok: Tok::Ident(ident.trim_start_matches('\\').to_string()),
                line,
            });
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            out.push(Token {
                tok: Tok::Num(chars[start..i].iter().collect()),
                line,
            });
            continue;
        }
        if let Some(p) = PUNCTS.iter().find(|p| starts_with(&chars, i, p)) {
            i += p.chars().count();
            out.push(Token {
                tok: Tok::Punct(p),
                line,
            });
            continue;
        }
        out.push(Token {
            tok: Tok::Other(c),
            line,
        });
        i += 1;
    }
    out
}

fn starts_with(chars: &[char], i: usize, pat: &str) -> bool {
    (i..)
        .zip(pat.chars())
        .all(|(j, p)| chars.get(j) == Some(&p))
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Collects `use Foo\Bar;` / `use Foo\Bar as Baz;` imports: short name -> FQCN.
pub fn use_imports(tokens: &[Token]) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    let mut i = 0;
    // Only top-level `use` statements appear before the first `{` in the files we read.
    while i < tokens.len() {
        if tokens[i].tok == Tok::Ident("use".into())
            && let Some(Tok::Ident(fqcn)) = tokens.get(i + 1).map(|t| &t.tok)
        {
            let mut short = fqcn.rsplit('\\').next().unwrap_or(fqcn).to_string();
            if let (Some(Tok::Ident(kw)), Some(Tok::Ident(alias))) = (
                tokens.get(i + 2).map(|t| &t.tok),
                tokens.get(i + 3).map(|t| &t.tok),
            ) && kw.eq_ignore_ascii_case("as")
            {
                short = alias.clone();
            }
            map.insert(short, fqcn.clone());
        }
        i += 1;
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_basic_php() {
        let toks = tokenize(
            "<?php\n// c\nRoute::get('/a', [X::class, 'b']); # h\n/* m\n */ $x = \"q\\n\";",
        );
        let kinds: Vec<_> = toks.iter().map(|t| t.tok.clone()).collect();
        assert_eq!(kinds[0], Tok::Ident("Route".into()));
        assert_eq!(kinds[1], Tok::Punct("::"));
        assert_eq!(kinds[4], Tok::Str("/a".into()));
        assert_eq!(toks[0].line, 3);
        assert!(kinds.contains(&Tok::Var("x".into())));
        assert!(kinds.contains(&Tok::Str("q\n".into())));
    }

    #[test]
    fn single_quote_escapes() {
        let toks = tokenize(r"'a\'b\\c\d'");
        assert_eq!(toks[0].tok, Tok::Str(r"a'b\c\d".into()));
    }

    #[test]
    fn collects_imports() {
        let toks = tokenize("<?php use App\\Http\\A; use B\\C as D;");
        let m = use_imports(&toks);
        assert_eq!(m["A"], "App\\Http\\A");
        assert_eq!(m["D"], "B\\C");
    }
}
