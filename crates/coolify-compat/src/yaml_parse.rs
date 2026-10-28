//! `Symfony\Component\Yaml\Yaml::parse($input, 0, maxAliasesForCollections: N)` (symfony/yaml 8.1,
//! the version upstream ships): `Parser`, `Inline` and `Unescaper` ported line by line, without the
//! flags upstream never passes (custom tags, objects, constants, datetimes).
//!
//! The result is a [`PhpValue`] as PHP would hold it: integer-like string keys become integer
//! keys, plain scalars are typed with Symfony's rules (`1_000` is an int, `2024-01-01` a unix
//! timestamp, `0644` a string, `yes` a string) and the error messages are those of
//! `ParseException::getMessage()`, line numbers included, because upstream shows them to users.

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::yaml::{Key, PhpValue, php_is_numeric};

/// `Parser::DEFAULT_MAX_NESTING_LEVEL`.
const MAX_NESTING_LEVEL: usize = 128;

/// `Symfony\Component\Yaml\Exception\ParseException`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    raw_message: String,
    parsed_line: i64,
    snippet: Option<String>,
}

impl ParseError {
    fn new(message: impl Into<String>, parsed_line: i64, snippet: Option<&str>) -> Self {
        Self {
            raw_message: message.into(),
            parsed_line,
            snippet: snippet.map(str::to_owned),
        }
    }

    fn bare(message: impl Into<String>) -> Self {
        Self::new(message, -1, None)
    }

    fn set_parsed_line(&mut self, line: i64) {
        self.parsed_line = line;
    }

    fn set_snippet(&mut self, snippet: &str) {
        self.snippet = Some(snippet.to_owned());
    }

    /// `ParseException::getMessage()` (`updateRepr()`).
    pub fn message(&self) -> String {
        let mut message = self.raw_message.clone();
        let dot = message.ends_with('.');
        if dot {
            message.pop();
        }
        if self.parsed_line >= 0 {
            message.push_str(&format!(" at line {}", self.parsed_line));
        }
        if let Some(s) = self.snippet.as_deref().filter(|s| php_truthy(s)) {
            message.push_str(&format!(" (near \"{s}\")"));
        }
        if dot {
            message.push('.');
        }
        message
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for ParseError {}

type Result<T> = std::result::Result<T, ParseError>;

/// `Yaml::parse($input, maxAliasesForCollections: $max_aliases)`.
pub fn parse(input: &str, max_aliases: usize) -> Result<PhpValue> {
    let mut ctx = Ctx {
        refs: HashMap::new(),
        nesting: 0,
        alias_count: 0,
        max_aliases,
        parsed_line_number: -1,
    };
    let mut parser = Parser::new();
    parser.do_parse(&mut ctx, input)
}

/// `Inline::parse($value)` on its own (a single flow value).
pub fn parse_inline(value: &str) -> Result<PhpValue> {
    let mut ctx = Ctx {
        refs: HashMap::new(),
        nesting: 0,
        alias_count: 0,
        max_aliases: 128,
        parsed_line_number: -1,
    };
    inline_parse(&mut ctx, value)
}

/// The shared state of one parse: the anchors (`$refs`, shared by reference between the nested
/// parsers and `Inline`), `ParserState` and `Inline::$parsedLineNumber`.
struct Ctx {
    refs: HashMap<String, PhpValue>,
    nesting: usize,
    alias_count: usize,
    max_aliases: usize,
    parsed_line_number: i64,
}

impl Ctx {
    /// `ParserState::enterNestingLevel()`.
    fn enter(&mut self, line: i64, snippet: Option<&str>) -> Result<()> {
        self.nesting += 1;
        if self.nesting > MAX_NESTING_LEVEL {
            self.nesting -= 1;
            return Err(ParseError::new(
                format!("Maximum nesting depth of {MAX_NESTING_LEVEL} exceeded."),
                line,
                snippet,
            ));
        }
        Ok(())
    }

    /// `ParserState::leaveNestingLevel()`.
    fn leave(&mut self) {
        self.nesting = self.nesting.saturating_sub(1);
    }

    /// `ParserState::countAlias()`.
    fn count_alias(&mut self, value: &PhpValue, line: i64, snippet: Option<&str>) -> Result<()> {
        if !matches!(value, PhpValue::Array(_)) {
            return Ok(());
        }
        self.alias_count += 1;
        if self.alias_count > self.max_aliases {
            return Err(ParseError::new(
                format!(
                    "Maximum number of collection aliases ({}) exceeded. This limit can be increased via the Parser constructor.",
                    self.max_aliases
                ),
                line,
                snippet,
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// PHP string helpers (byte semantics)

/// PHP truthiness of a string (`''` and `'0'` are false).
fn php_truthy(s: &str) -> bool {
    !s.is_empty() && s != "0"
}

const PHP_TRIM: &[char] = &[' ', '\t', '\n', '\r', '\0', '\x0B'];

fn trim(s: &str) -> &str {
    s.trim_matches(PHP_TRIM)
}

fn rtrim(s: &str) -> &str {
    s.trim_end_matches(PHP_TRIM)
}

/// `substr($s, $start)` with PHP's clamping (byte offsets).
fn substr_from(s: &str, start: usize) -> String {
    let b = s.as_bytes();
    if start >= b.len() {
        return String::new();
    }
    String::from_utf8_lossy(&b[start..]).into_owned()
}

/// `substr($s, $start, $len)` (byte offsets, clamped).
fn substr(s: &str, start: usize, len: usize) -> String {
    let b = s.as_bytes();
    if start >= b.len() {
        return String::new();
    }
    let end = start.saturating_add(len).min(b.len());
    String::from_utf8_lossy(&b[start..end]).into_owned()
}

fn byte_at(s: &str, i: usize) -> Option<u8> {
    s.as_bytes().get(i).copied()
}

/// `strspn($s, $chars, $start)`.
fn strspn(s: &str, chars: &[u8], start: usize) -> usize {
    s.as_bytes()
        .iter()
        .skip(start)
        .take_while(|b| chars.contains(b))
        .count()
}

/// `strcspn($s, $chars, $start)`.
fn strcspn(s: &str, chars: &[u8], start: usize) -> usize {
    s.as_bytes()
        .iter()
        .skip(start)
        .take_while(|b| !chars.contains(b))
        .count()
}

/// PCRE `\s` (ASCII part: space, \t, \n, \v, \f, \r).
fn is_pcre_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `preg_replace('/\s*(#.*)?$/A', '', $s)`: what is left after the leading whitespace and an
/// optional comment (non-empty means unexpected characters).
fn leftover_after_comment(s: &str) -> String {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && is_pcre_space(b[i]) {
        i += 1;
    }
    // `\s*` is greedy, `$` matches at the end or before a final newline.
    let try_end = |pos: usize| -> Option<usize> {
        if pos == b.len() || (pos + 1 == b.len() && b[pos] == b'\n') {
            Some(pos)
        } else {
            None
        }
    };
    let comment_end = |start: usize| -> Option<usize> {
        if b.get(start) != Some(&b'#') {
            return None;
        }
        let mut j = start + 1;
        while j < b.len() && b[j] != b'\n' {
            j += 1;
        }
        try_end(j)
    };
    // Backtracking over `\s*` from the longest run.
    let mut k = i;
    loop {
        if let Some(end) = comment_end(k).or_else(|| try_end(k)) {
            return substr_from(s, end);
        }
        if k == 0 {
            break;
        }
        k -= 1;
    }
    s.to_owned()
}

fn re(pattern: &str) -> fancy_regex::Regex {
    fancy_regex::Regex::new(pattern).unwrap_or_else(|e| panic!("invalid regex {pattern}: {e}"))
}

fn is_match(r: &fancy_regex::Regex, s: &str) -> bool {
    r.is_match(s).unwrap_or(false)
}

fn captures<'a>(r: &fancy_regex::Regex, s: &'a str) -> Option<fancy_regex::Captures<'a>> {
    r.captures(s).ok().flatten()
}

const TAG_PATTERN: &str = r"(?P<tag>![\w!./:-]+)";
const BLOCK_SCALAR_HEADER_PATTERN: &str =
    r"(?P<separator>\||>)(?P<modifiers>\+|\-|\d+|\+\d+|\-\d+|\d+\+|\d+\-)?(?P<comments>[ \t]+#.*)?";
const REGEX_QUOTED_STRING: &str =
    r#"(?:"(?:[^"\\]*+(?:\\.[^"\\]*+)*+)"|'(?:[^']*+(?:''[^']*+)*+)')"#;

static SEQUENCE_ITEM: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| re(r"^\-((?P<leadspaces>\s+)(?P<value>.+))?$"));
static REFERENCE: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| re(r"^&(?P<ref>[^ ]++) *+(?P<value>.*)"));
static SEQUENCE_MAPPING: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    re(&format!(
        r#"^(?P<key>{REGEX_QUOTED_STRING}|[^ '"\{{\[].*?) *\:(\s+(?P<value>.+?))?\s*$"#
    ))
});
static MAPPING_KEY: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    re(&format!(
        r#"^(?P<key>(?:![^\s]++\s++)?(?:{REGEX_QUOTED_STRING}|[^ '"\[\{{!].*?)|![^\s]++(?= +\:)) *\:(( |\t)++(?P<value>.+))?$"#
    ))
});
static TAG_BLOCK_SCALAR: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    re(&format!(
        r"^{TAG_PATTERN}[ \t]+{BLOCK_SCALAR_HEADER_PATTERN}$"
    ))
});
static OPTIONAL_TAG_BLOCK_SCALAR: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    re(&format!(
        r"^(?:{TAG_PATTERN}[ \t]+)?{BLOCK_SCALAR_HEADER_PATTERN}$"
    ))
});
static NEXT_LINE_BLOCK_SCALAR: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
    re(&format!(
        r":[ \t]+(?:{TAG_PATTERN}[ \t]+)?{BLOCK_SCALAR_HEADER_PATTERN}$"
    ))
});
static LINE_TAG: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| re(&format!(r"^{TAG_PATTERN} *( +#.*)?$")));
static ANCHOR_AFTER_TAG: LazyLock<fancy_regex::Regex> =
    LazyLock::new(|| re(&format!(r"^{TAG_PATTERN} ++(&[^ ]++)")));
static COMMENT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[ \t]+#").unwrap_or_else(|_| unreachable!()));

/// The offset of the first `/[ \t]+#/` match.
fn comment_start(s: &str) -> Option<usize> {
    COMMENT.find(s).map(|m| m.start())
}

// ---------------------------------------------------------------------------------------------
// Parser

struct Parser {
    offset: usize,
    number_of_parsed_lines: usize,
    total_number_of_lines: Option<usize>,
    lines: Vec<String>,
    current_line_nb: isize,
    current_line: String,
    skipped_line_numbers: Vec<usize>,
    refs_being_parsed: Vec<String>,
}

enum Context {
    Sequence,
    Mapping,
}

impl Parser {
    fn new() -> Self {
        Self {
            offset: 0,
            number_of_parsed_lines: 0,
            total_number_of_lines: None,
            lines: Vec::new(),
            current_line_nb: -1,
            current_line: String::new(),
            skipped_line_numbers: Vec::new(),
            refs_being_parsed: Vec::new(),
        }
    }

    fn real_line(&self) -> i64 {
        let mut n = self.current_line_nb as i64 + self.offset as i64;
        for &skipped in &self.skipped_line_numbers {
            if skipped as i64 > n {
                break;
            }
            n += 1;
        }
        n
    }

    fn cur(&self, i: usize) -> Option<u8> {
        byte_at(&self.current_line, i)
    }

    #[allow(clippy::too_many_lines)]
    fn do_parse(&mut self, ctx: &mut Ctx, value: &str) -> Result<PhpValue> {
        self.current_line_nb = -1;
        self.current_line = String::new();
        let (value, has_content_on_start_marker_line) = self.cleanup(value)?;
        // `$value` is reused by the mapping branch; the 1-liner check below reads it.
        let mut php_value: Option<String> = Some(value.clone());
        self.lines = value.split('\n').map(str::to_owned).collect();
        self.number_of_parsed_lines = self.lines.len();
        if self.total_number_of_lines.is_none() {
            self.total_number_of_lines = Some(self.number_of_parsed_lines);
        }

        if !self.move_to_next_line() {
            return Ok(PhpValue::Null);
        }

        let mut data: Vec<(Key, PhpValue)> = Vec::new();
        let mut context: Option<Context> = None;
        let mut allow_overwrite = false;

        while self.is_current_line_empty() {
            if !self.move_to_next_line() {
                return Ok(PhpValue::Null);
            }
        }

        // Resolves the tag and returns if end of the document
        let line = self.current_line.clone();
        if self.get_line_tag(&line, false)?.is_some() {
            // Custom tags are not enabled: get_line_tag() threw for every other tag.
            unreachable_tag();
        }

        loop {
            'body: {
                if self.is_current_line_empty() {
                    break 'body;
                }
                if self.cur(0) == Some(b'\t') {
                    return Err(ParseError::new(
                        "A YAML file cannot contain tabs as indentation.",
                        self.real_line() + 1,
                        Some(&self.current_line.clone()),
                    ));
                }
                ctx.parsed_line_number = self.real_line();

                let mut is_ref: Option<String> = None;
                let mut merge_node = false;
                let trimmed_line = rtrim(&self.current_line).to_owned();
                let seq = if self.cur(0) == Some(b'-') {
                    captures(&SEQUENCE_ITEM, &trimmed_line).map(|c| {
                        (
                            c.name("leadspaces").map(|m| m.as_str().to_owned()),
                            c.name("value").map(|m| m.as_str().to_owned()),
                        )
                    })
                } else {
                    None
                };
                if let Some((leadspaces, mut value)) = seq {
                    if matches!(context, Some(Context::Mapping)) {
                        return Err(ParseError::new(
                            "You cannot define a sequence item when in a mapping.",
                            self.real_line() + 1,
                            Some(&self.current_line.clone()),
                        ));
                    }
                    context = Some(Context::Sequence);
                    if has_content_on_start_marker_line && self.current_line_nb == 0 {
                        return Err(ParseError::new(
                            "A block collection cannot start on the line of the document start marker (---).",
                            self.real_line() + 1,
                            Some(&self.current_line.clone()),
                        ));
                    }
                    if let Some(v) = value.as_mut()
                        && v.starts_with('!')
                    {
                        *v = move_anchor_before_tag(v);
                    }
                    if let Some(v) = value.clone()
                        && v.starts_with('&')
                        && let Some(c) = captures(&REFERENCE, &v)
                    {
                        let r = c
                            .name("ref")
                            .map(|m| m.as_str().to_owned())
                            .unwrap_or_default();
                        self.refs_being_parsed.push(r.clone());
                        is_ref = Some(r);
                        value = Some(
                            c.name("value")
                                .map(|m| m.as_str().to_owned())
                                .unwrap_or_default(),
                        );
                    }
                    if let Some(v) = value.as_deref()
                        && v.len() > 1
                        && v.starts_with("? ")
                    {
                        return Err(ParseError::new(
                            "Complex mappings are not supported.",
                            self.real_line() + 1,
                            Some(&self.current_line.clone()),
                        ));
                    }
                    let item;
                    if let Some(v) = value.as_deref()
                        && v.trim_start_matches(' ').starts_with('-')
                    {
                        // Inline first child
                        let current_line_number = self.real_line();
                        let sequence_indentation = leadspaces.as_deref().map_or(0, str::len) + 1;
                        let mut sequence_yaml =
                            substr_from(&self.current_line, sequence_indentation);
                        let embed = self.get_next_embed_block(Some(sequence_indentation), true)?;
                        if current_line_number != self.real_line() {
                            sequence_yaml.push('\n');
                            sequence_yaml.push_str(&embed);
                        }
                        item = self.parse_block(ctx, current_line_number, &sequence_yaml)?;
                    } else if value.as_deref().is_none_or(|v| {
                        v.trim_matches(' ').is_empty() || v.trim_start_matches(' ').starts_with('#')
                    }) {
                        let offset = self.real_line() + 1;
                        let embed = self.get_next_embed_block(None, true)?;
                        item = self.parse_block(ctx, offset, &embed)?;
                    } else {
                        let v = value.clone().unwrap_or_default();
                        if self
                            .get_line_tag(v.trim_start_matches(' '), true)?
                            .is_some()
                        {
                            unreachable_tag();
                        }
                        if is_match(&TAG_BLOCK_SCALAR, &v) {
                            item = self.parse_value(ctx, &v, &Context::Sequence)?;
                        } else if leadspaces.is_some()
                            && (v.starts_with('!') || is_match(&SEQUENCE_MAPPING, &trim_tag(&v)))
                        {
                            let current_line_number = self.real_line();
                            let mut block = v.clone();
                            if self.is_next_line_indented()
                                || self.is_next_line_in_block_scalar(&block)
                            {
                                block.push('\n');
                                let indentation = self.current_line_indentation()
                                    + leadspaces.as_deref().map_or(0, str::len)
                                    + 1;
                                block.push_str(
                                    &self.get_next_embed_block(Some(indentation), false)?,
                                );
                            }
                            item = self.parse_block(ctx, current_line_number, &block)?;
                        } else {
                            item = self.parse_value(ctx, &v, &Context::Sequence)?;
                        }
                    }
                    let mut arr = PhpValue::Array(std::mem::take(&mut data));
                    arr.push(item.clone());
                    if let PhpValue::Array(items) = arr {
                        data = items;
                    }
                    if let Some(r) = is_ref.take() {
                        ctx.refs.insert(r, item);
                        self.refs_being_parsed.pop();
                    }
                    break 'body;
                }

                let mapping = captures(&MAPPING_KEY, &trimmed_line).and_then(|c| {
                    let key = c
                        .name("key")
                        .map(|m| m.as_str().to_owned())
                        .unwrap_or_default();
                    let value = c.name("value").map(|m| m.as_str().to_owned());
                    (!key.contains(" #") || key.starts_with('"') || key.starts_with('\''))
                        .then_some((key, value))
                });
                if let Some((key_text, mut value)) = mapping {
                    if matches!(context, Some(Context::Sequence)) {
                        return Err(ParseError::new(
                            "You cannot define a mapping item when in a sequence.",
                            self.current_line_nb as i64 + 1,
                            Some(&self.current_line.clone()),
                        ));
                    }
                    context = Some(Context::Mapping);
                    if has_content_on_start_marker_line && self.current_line_nb == 0 {
                        return Err(ParseError::new(
                            "A block collection cannot start on the line of the document start marker (---).",
                            self.real_line() + 1,
                            Some(&self.current_line.clone()),
                        ));
                    }
                    if let Some(v) = value.as_mut()
                        && v.starts_with('!')
                    {
                        *v = move_anchor_before_tag(v);
                    }
                    let mut i = 0;
                    let mut quoted = false;
                    let key = match parse_scalar(ctx, &key_text, None, &mut i, true, &mut quoted) {
                        Ok(k) => k,
                        Err(mut e) => {
                            e.set_parsed_line(self.real_line() + 1);
                            e.set_snippet(&self.current_line);
                            return Err(e);
                        }
                    };
                    let key = match key {
                        PhpValue::Str(s) => Key::from_php_string(&s),
                        PhpValue::Int(n) => Key::Int(n),
                        other => {
                            let numeric = matches!(other, PhpValue::Int(_) | PhpValue::Float(_));
                            return Err(ParseError::new(
                                format!(
                                    "{} keys are not supported. Quote your evaluable mapping keys instead.",
                                    if numeric { "Numeric" } else { "Non-string" }
                                ),
                                self.real_line() + 1,
                                Some(&self.current_line.clone()),
                            ));
                        }
                    };
                    let is_merge_key = key == Key::Str("<<".into());
                    let mut merge_ref: Option<String> = None;
                    if is_merge_key {
                        merge_ref = value
                            .as_deref()
                            .filter(|v| v.starts_with('&'))
                            .and_then(|v| {
                                let rest = &v[1..];
                                let r: String = rest.chars().take_while(|c| *c != ' ').collect();
                                (!r.is_empty()).then_some(r)
                            });
                    }
                    if is_merge_key && merge_ref.is_none() {
                        merge_node = true;
                        allow_overwrite = true;
                        if let Some(v) = value.as_deref()
                            && v.starts_with('*')
                        {
                            let mut ref_name = substr_from(rtrim(v), 1);
                            if let Some(pos) = comment_start(&ref_name) {
                                ref_name.truncate(pos);
                            }
                            let Some(ref_value) = ctx.refs.get(&ref_name).cloned() else {
                                if let Some(pos) =
                                    self.refs_being_parsed.iter().position(|r| *r == ref_name)
                                {
                                    let mut chain: Vec<String> =
                                        self.refs_being_parsed[pos..].to_vec();
                                    chain.push(ref_name.clone());
                                    return Err(ParseError::new(
                                        format!(
                                            "Circular reference [{}] detected for reference \"{ref_name}\".",
                                            chain.join(", ")
                                        ),
                                        self.current_line_nb as i64 + 1,
                                        Some(&self.current_line.clone()),
                                    ));
                                }
                                return Err(ParseError::new(
                                    format!("Reference \"{ref_name}\" does not exist."),
                                    self.real_line() + 1,
                                    Some(&self.current_line.clone()),
                                ));
                            };
                            ctx.count_alias(
                                &ref_value,
                                self.real_line() + 1,
                                Some(&self.current_line.clone()),
                            )?;
                            let PhpValue::Array(items) = ref_value else {
                                return Err(ParseError::new(
                                    "YAML merge keys used with a scalar value instead of an array.",
                                    self.real_line() + 1,
                                    Some(&self.current_line.clone()),
                                ));
                            };
                            array_union(&mut data, items);
                        } else {
                            let current_line_number = self.real_line();
                            let block = match value.as_deref() {
                                Some(v) if !v.is_empty() => v.to_owned(),
                                _ => self.get_next_embed_block(None, false)?,
                            };
                            php_value = Some(block.clone());
                            let parsed = self.parse_block(ctx, current_line_number + 1, &block)?;
                            let PhpValue::Array(items) = parsed else {
                                return Err(ParseError::new(
                                    "YAML merge keys used with a scalar value instead of an array.",
                                    self.real_line() + 1,
                                    Some(&self.current_line.clone()),
                                ));
                            };
                            if items
                                .iter()
                                .any(|(k, v)| *k == Key::Int(0) && *v != PhpValue::Null)
                            {
                                for (_, parsed_item) in items {
                                    let PhpValue::Array(sub) = parsed_item else {
                                        return Err(ParseError::new(
                                            "Merge items must be arrays.",
                                            self.real_line() + 1,
                                            None,
                                        ));
                                    };
                                    array_union(&mut data, sub);
                                }
                            } else {
                                array_union(&mut data, items);
                            }
                        }
                    } else if !is_merge_key
                        && let Some(v) = value.clone()
                        && v.starts_with('&')
                        && let Some(c) = captures(&REFERENCE, &v)
                    {
                        let r = c
                            .name("ref")
                            .map(|m| m.as_str().to_owned())
                            .unwrap_or_default();
                        self.refs_being_parsed.push(r.clone());
                        is_ref = Some(r);
                        value = Some(
                            c.name("value")
                                .map(|m| m.as_str().to_owned())
                                .unwrap_or_default(),
                        );
                    }

                    if merge_node {
                        // Merge keys
                    } else if value
                        .as_deref()
                        .is_none_or(|v| v.is_empty() || v.starts_with('#'))
                        || self
                            .get_line_tag(value.as_deref().unwrap_or_default(), true)?
                            .is_some()
                        || is_merge_key
                    {
                        // hash
                        if !self.is_next_line_indented()
                            && !self.is_next_line_unindented_collection()
                        {
                            if allow_overwrite || !isset(&data, &key) {
                                if !allow_overwrite && array_key_exists(&data, &key) {
                                    return Err(duplicate_key(
                                        &key,
                                        self.real_line() + 1,
                                        &self.current_line,
                                    ));
                                }
                                set_key(&mut data, key.clone(), PhpValue::Null);
                            } else {
                                return Err(duplicate_key(
                                    &key,
                                    self.real_line() + 1,
                                    &self.current_line,
                                ));
                            }
                        } else {
                            let real_current_line_nb_key = self.real_line();
                            let offset = self.real_line() + 1;
                            let embed = self.get_next_embed_block(None, false)?;
                            let parsed = self.parse_block(ctx, offset, &embed)?;
                            php_value = match &parsed {
                                PhpValue::Str(s) => Some(s.clone()),
                                _ => None,
                            };
                            if is_merge_key {
                                if let Some(r) = merge_ref.clone() {
                                    ctx.refs.insert(r, parsed.clone());
                                }
                                if let PhpValue::Array(items) = parsed {
                                    array_union(&mut data, items);
                                }
                            } else if allow_overwrite || !isset(&data, &key) {
                                if !allow_overwrite && array_key_exists(&data, &key) {
                                    return Err(duplicate_key(
                                        &key,
                                        self.real_line() + 1,
                                        &self.current_line,
                                    ));
                                }
                                set_key(&mut data, key.clone(), parsed);
                            } else {
                                return Err(duplicate_key(
                                    &key,
                                    real_current_line_nb_key + 1,
                                    &self.current_line,
                                ));
                            }
                        }
                    } else {
                        let v = value.clone().unwrap_or_default();
                        let parsed = self.parse_value(ctx, rtrim(&v), &Context::Mapping)?;
                        php_value = match &parsed {
                            PhpValue::Str(s) => Some(s.clone()),
                            _ => None,
                        };
                        if allow_overwrite || !isset(&data, &key) {
                            if !allow_overwrite && array_key_exists(&data, &key) {
                                return Err(duplicate_key(
                                    &key,
                                    self.real_line() + 1,
                                    &self.current_line,
                                ));
                            }
                            set_key(&mut data, key.clone(), parsed);
                        } else {
                            return Err(duplicate_key(
                                &key,
                                self.real_line() + 1,
                                &self.current_line,
                            ));
                        }
                    }
                    if let Some(r) = is_ref.take() {
                        let v = data
                            .iter()
                            .find(|(k, _)| *k == key)
                            .map(|(_, v)| v.clone())
                            .unwrap_or(PhpValue::Null);
                        ctx.refs.insert(r, v);
                        self.refs_being_parsed.pop();
                    }
                    break 'body;
                }

                let first = self.cur(0);
                if first == Some(b'"') || first == Some(b'\'') {
                    if context.is_some() {
                        return Err(ParseError::new(
                            "Unable to parse.",
                            self.real_line() + 1,
                            Some(&self.current_line.clone()),
                        ));
                    }
                    let mut cursor = 0;
                    let lexed = match self.lex_inline_quoted_string(&mut cursor) {
                        Ok(s) => s,
                        Err(mut e) => {
                            e.set_parsed_line(self.real_line() + 1);
                            e.set_snippet(&self.current_line);
                            return Err(e);
                        }
                    };
                    return inline_parse(ctx, &lexed).map_err(|mut e| {
                        e.set_parsed_line(self.real_line() + 1);
                        e.set_snippet(&self.current_line);
                        e
                    });
                }
                if first == Some(b'{') || first == Some(b'[') {
                    if context.is_some() {
                        return Err(ParseError::new(
                            "Unable to parse.",
                            self.real_line() + 1,
                            Some(&self.current_line.clone()),
                        ));
                    }
                    let closing = if first == Some(b'{') { b'}' } else { b']' };
                    let result = (|| -> Result<PhpValue> {
                        let mut cursor = 0;
                        let lexed = self.lex_inline_structure(ctx, &mut cursor, closing, true)?;
                        let parsed = inline_parse(ctx, &lexed)?;
                        while self.move_to_next_line() {
                            if !self.is_current_line_empty() {
                                return Err(ParseError::new(
                                    "Unable to parse.",
                                    self.real_line() + 1,
                                    Some(&self.current_line.clone()),
                                ));
                            }
                        }
                        Ok(parsed)
                    })();
                    return result.map_err(|mut e| {
                        e.set_parsed_line(self.real_line() + 1);
                        e.set_snippet(&self.current_line);
                        e
                    });
                }
                if self.current_line.len() > 1 && self.current_line.starts_with("? ") {
                    return Err(ParseError::new(
                        "Complex mappings are not supported.",
                        self.real_line() + 1,
                        Some(&self.current_line.clone()),
                    ));
                }
                // 1-liner optionally followed by newline(s): `$value` is the whole input here.
                if let Some(v) = php_value.as_deref()
                    && self.lines.first().map(String::as_str) == Some(trim(v))
                {
                    let line0 = self.lines[0].clone();
                    return inline_parse(ctx, &line0).map_err(|mut e| {
                        e.set_parsed_line(self.real_line() + 1);
                        e.set_snippet(&self.current_line);
                        e
                    });
                }
                // try to parse the value as a multi-line string as a last resort
                if self.current_line_nb == 0 {
                    let mut previous_line_was_newline = false;
                    let mut previous_line_was_terminated_with_backslash = false;
                    let mut joined = String::new();
                    for line in self.lines.clone() {
                        let trimmed_line = trim(&line);
                        if trimmed_line.starts_with('#') {
                            continue;
                        }
                        if self.offset == 0 && line.starts_with(' ') {
                            return Err(ParseError::new(
                                "Unable to parse.",
                                self.real_line() + 1,
                                Some(&self.current_line.clone()),
                            ));
                        }
                        if line.contains(": ") {
                            return Err(ParseError::new(
                                "Mapping values are not allowed in multi-line blocks.",
                                self.real_line() + 1,
                                Some(&self.current_line.clone()),
                            ));
                        }
                        if trimmed_line.is_empty() {
                            joined.push('\n');
                        } else if !previous_line_was_newline
                            && !previous_line_was_terminated_with_backslash
                        {
                            joined.push(' ');
                        }
                        if !trimmed_line.is_empty() && line.ends_with('\\') {
                            joined.push_str(
                                substr(&line, 0, line.len() - 1).trim_start_matches(PHP_TRIM),
                            );
                        } else if !trimmed_line.is_empty() {
                            joined.push_str(trimmed_line);
                        }
                        if trimmed_line.is_empty() {
                            previous_line_was_newline = true;
                            previous_line_was_terminated_with_backslash = false;
                        } else if line.ends_with('\\') {
                            previous_line_was_newline = false;
                            previous_line_was_terminated_with_backslash = true;
                        } else {
                            previous_line_was_newline = false;
                            previous_line_was_terminated_with_backslash = false;
                        }
                    }
                    if let Ok(v) = inline_parse(ctx, trim(&joined)) {
                        return Ok(v);
                    }
                }
                return Err(ParseError::new(
                    "Unable to parse.",
                    self.real_line() + 1,
                    Some(&self.current_line.clone()),
                ));
            }
            if !self.move_to_next_line() {
                break;
            }
        }

        if data.is_empty() {
            return Ok(PhpValue::Null);
        }
        Ok(PhpValue::Array(data))
    }

    fn parse_block(&mut self, ctx: &mut Ctx, offset: i64, yaml: &str) -> Result<PhpValue> {
        let mut parser = Parser::new();
        parser.offset = usize::try_from(offset).unwrap_or(0);
        parser.total_number_of_lines = self.total_number_of_lines;
        parser.skipped_line_numbers = self.skipped_line_numbers.clone();
        parser.refs_being_parsed = self.refs_being_parsed.clone();
        ctx.enter(offset + 1, Some(&self.current_line.clone()))?;
        let r = parser.do_parse(ctx, yaml);
        ctx.leave();
        r
    }

    fn current_line_indentation(&self) -> usize {
        if self.cur(0) != Some(b' ') {
            return 0;
        }
        self.current_line.len() - self.current_line.trim_start_matches(' ').len()
    }

    fn get_next_embed_block(
        &mut self,
        indentation: Option<usize>,
        in_sequence: bool,
    ) -> Result<String> {
        let old_line_indentation = self.current_line_indentation();
        if !self.move_to_next_line() {
            return Ok(String::new());
        }
        let new_indent: Option<usize>;
        if let Some(i) = indentation {
            new_indent = Some(i);
        } else {
            let mut found: Option<usize> = None;
            let mut movements = 0;
            loop {
                let mut eof = false;
                if self.is_current_line_empty() || self.is_current_line_comment() {
                    eof = !self.move_to_next_line();
                    if !eof {
                        movements += 1;
                    }
                } else {
                    found = Some(self.current_line_indentation());
                }
                if eof || found.is_some() {
                    break;
                }
            }
            for _ in 0..movements {
                self.move_to_previous_line();
            }
            let unindented_embed_block = self.is_string_unindented_collection_item();
            if !self.is_current_line_empty() && found == Some(0) && !unindented_embed_block {
                return Err(ParseError::new(
                    "Indentation problem.",
                    self.real_line() + 1,
                    Some(&self.current_line.clone()),
                ));
            }
            new_indent = found;
        }
        let ni = new_indent.unwrap_or(0);
        let mut data: Vec<String> = Vec::new();
        // `getCurrentLineIndentation() >= $newIndent` with a null indent compares with 0.
        if self.current_line_indentation() >= ni {
            data.push(substr_from(&self.current_line, ni));
        } else if self.is_current_line_empty() || self.is_current_line_comment() {
            data.push(self.current_line.clone());
        } else {
            self.move_to_previous_line();
            return Ok(String::new());
        }
        if in_sequence && new_indent == Some(old_line_indentation) && data[0].starts_with('-') {
            self.move_to_previous_line();
            return Ok(String::new());
        }
        let mut is_it_unindented_collection = self.is_string_unindented_collection_item();
        let mut is_it_comment = self.is_current_line_comment();
        while self.move_to_next_line() {
            if is_it_comment && !is_it_unindented_collection {
                is_it_unindented_collection = self.is_string_unindented_collection_item();
                is_it_comment = self.is_current_line_comment();
            }
            let indent = self.current_line_indentation();
            if is_it_unindented_collection
                && !self.is_current_line_empty()
                && !self.is_string_unindented_collection_item()
                && new_indent == Some(indent)
            {
                self.move_to_previous_line();
                break;
            }
            if self.is_current_line_blank() {
                data.push(substr_from(&self.current_line, ni));
                continue;
            }
            if indent >= ni {
                data.push(substr_from(&self.current_line, ni));
            } else if self.is_current_line_comment() {
                data.push(self.current_line.clone());
            } else if indent == 0 {
                self.move_to_previous_line();
                break;
            } else {
                return Err(ParseError::new(
                    "Indentation problem.",
                    self.real_line() + 1,
                    Some(&self.current_line.clone()),
                ));
            }
        }
        Ok(data.join("\n"))
    }

    fn has_more_lines(&self) -> bool {
        (self.lines.len() as isize - 1) > self.current_line_nb
    }

    fn move_to_next_line(&mut self) -> bool {
        if self.current_line_nb >= self.number_of_parsed_lines as isize - 1 {
            return false;
        }
        self.current_line_nb += 1;
        self.current_line = self.lines[self.current_line_nb as usize].clone();
        true
    }

    fn move_to_previous_line(&mut self) -> bool {
        if self.current_line_nb < 1 {
            return false;
        }
        self.current_line_nb -= 1;
        self.current_line = self.lines[self.current_line_nb as usize].clone();
        true
    }

    #[allow(clippy::too_many_lines)]
    fn parse_value(&mut self, ctx: &mut Ctx, value: &str, context: &Context) -> Result<PhpValue> {
        if let Some(rest) = value.strip_prefix('*') {
            let mut name = rest.to_owned();
            if let Some(pos) = comment_start(&name) {
                name.truncate(pos);
            }
            let Some(found) = ctx.refs.get(&name).cloned() else {
                if let Some(pos) = self.refs_being_parsed.iter().position(|r| *r == name) {
                    let mut chain: Vec<String> = self.refs_being_parsed[pos..].to_vec();
                    chain.push(name.clone());
                    return Err(ParseError::new(
                        format!(
                            "Circular reference [{}] detected for reference \"{name}\".",
                            chain.join(", ")
                        ),
                        self.current_line_nb as i64 + 1,
                        Some(&self.current_line.clone()),
                    ));
                }
                return Err(ParseError::new(
                    format!("Reference \"{name}\" does not exist."),
                    self.current_line_nb as i64 + 1,
                    Some(&self.current_line.clone()),
                ));
            };
            ctx.count_alias(
                &found,
                self.real_line() + 1,
                Some(&self.current_line.clone()),
            )?;
            return Ok(found);
        }

        if matches!(byte_at(value, 0), Some(b'!' | b'|' | b'>'))
            && let Some(c) = captures(&OPTIONAL_TAG_BLOCK_SCALAR, value)
        {
            let separator = c
                .name("separator")
                .map(|m| m.as_str().to_owned())
                .unwrap_or_default();
            let modifiers = c
                .name("modifiers")
                .map(|m| m.as_str().to_owned())
                .unwrap_or_default();
            let tag = c
                .name("tag")
                .map(|m| m.as_str().to_owned())
                .unwrap_or_default();
            let chomping: String = modifiers
                .chars()
                .filter(|ch| !ch.is_ascii_digit())
                .collect();
            let indentation = php_int_prefix(&modifiers).unsigned_abs() as usize;
            let data = self.parse_block_scalar(&separator, &chomping, indentation);
            if !tag.is_empty() && tag != "!" {
                return match tag.as_str() {
                    "!!binary" => evaluate_binary_scalar(ctx, &data).map(PhpValue::Str),
                    "!!str" => Ok(PhpValue::Str(data)),
                    "!!float" => Ok(PhpValue::Float(php_float_value(&data))),
                    _ => {
                        // A custom tag: only reachable with PARSE_CUSTOM_TAGS.
                        Ok(PhpValue::Str(data))
                    }
                };
            }
            return Ok(PhpValue::Str(data));
        }

        let result = (|| -> Result<PhpValue> {
            let first = byte_at(value, 0);
            if first == Some(b'{') || first == Some(b'[') {
                let mut cursor = rtrim(&self.current_line).len() - rtrim(value).len();
                let closing = if first == Some(b'{') { b'}' } else { b']' };
                let lexed = self.lex_inline_structure(ctx, &mut cursor, closing, true)?;
                return inline_parse(ctx, &lexed);
            }
            match first {
                Some(b'"' | b'\'') => {
                    let mut cursor = rtrim(&self.current_line).len() - rtrim(value).len();
                    let lexed = self.lex_inline_quoted_string(&mut cursor)?;
                    let parsed = inline_parse(ctx, &lexed)?;
                    if cursor < self.current_line.len() {
                        let rest = substr_from(&self.current_line, cursor);
                        if php_truthy(&leftover_after_comment(&rest)) {
                            return Err(ParseError::bare(format!(
                                "Unexpected characters near \"{rest}\"."
                            )));
                        }
                    }
                    Ok(parsed)
                }
                _ => {
                    let mut lines: Vec<String> = Vec::new();
                    while self.move_to_next_line() {
                        // unquoted strings end before the first unindented line
                        if self.current_line_indentation() == 0 {
                            self.move_to_previous_line();
                            break;
                        }
                        if self.is_current_line_comment() {
                            break;
                        }
                        if matches!(context, Context::Mapping)
                            && self.current_line.contains(": ")
                            && !self.is_current_line_comment()
                        {
                            return Err(ParseError::new(
                                "A colon cannot be used in an unquoted mapping value.",
                                self.real_line() + 1,
                                Some(&self.current_line.clone()),
                            ));
                        }
                        lines.push(trim(&self.current_line).to_owned());
                    }
                    let mut joined = value.to_owned();
                    let mut previous_line_blank = false;
                    for l in &lines {
                        if l.is_empty() {
                            joined.push('\n');
                            previous_line_blank = true;
                        } else if previous_line_blank {
                            joined.push_str(l);
                            previous_line_blank = false;
                        } else {
                            joined.push(' ');
                            joined.push_str(l);
                            previous_line_blank = false;
                        }
                    }
                    ctx.parsed_line_number = self.real_line();
                    let parsed = inline_parse(ctx, &joined)?;
                    if matches!(context, Context::Mapping)
                        && let PhpValue::Str(s) = &parsed
                        && !matches!(byte_at(&joined, 0), Some(b'"' | b'\'' | b'[' | b'{' | b'!'))
                        && s.contains(": ")
                    {
                        return Err(ParseError::new(
                            "A colon cannot be used in an unquoted mapping value.",
                            self.real_line() + 1,
                            Some(&joined),
                        ));
                    }
                    Ok(parsed)
                }
            }
        })();
        result.map_err(|mut e| {
            e.set_parsed_line(self.real_line() + 1);
            e.set_snippet(&self.current_line);
            e
        })
    }

    fn parse_block_scalar(&mut self, style: &str, chomping: &str, indentation: usize) -> String {
        let mut indentation = indentation;
        let mut not_eof = self.move_to_next_line();
        if !not_eof {
            return String::new();
        }
        let mut is_current_line_blank = self.is_current_line_blank();
        let mut block_lines: Vec<String> = Vec::new();
        let mut longest_blank_line = 0;

        // leading blank lines are consumed before determining indentation
        while not_eof
            && is_current_line_blank
            && (indentation == 0 || self.current_line.len() <= indentation)
        {
            longest_blank_line = longest_blank_line.max(self.current_line.len());
            not_eof = self.move_to_next_line();
            if not_eof {
                block_lines.push(String::new());
                is_current_line_blank = self.is_current_line_blank();
            }
        }

        // determine indentation if not specified
        if indentation == 0 {
            indentation = self.current_line.bytes().take_while(|b| *b == b' ').count();
            if indentation < longest_blank_line && self.is_current_line_comment() {
                indentation = longest_blank_line;
            }
        }

        if !not_eof || indentation == 0 || strspn(&self.current_line, b" ", 0) < indentation {
            if not_eof {
                self.move_to_previous_line();
            } else if !self.is_current_line_last_line_in_document() {
                block_lines.push(String::new());
            }
            // without content, the line breaks of the empty lines are only kept with the "+" chomping indicator
            return if chomping == "+" {
                "\n".repeat(block_lines.len())
            } else {
                String::new()
            };
        }

        loop {
            if !not_eof {
                break;
            }
            let matched = !is_current_line_blank
                && self.current_line.len() >= indentation
                && self.current_line.as_bytes()[..indentation]
                    .iter()
                    .all(|b| *b == b' ');
            if !(is_current_line_blank || matched) {
                break;
            }
            if is_current_line_blank && self.current_line.len() > indentation {
                block_lines.push(substr_from(&self.current_line, indentation));
            } else if is_current_line_blank {
                block_lines.push(String::new());
            } else {
                block_lines.push(substr_from(&self.current_line, indentation));
            }
            not_eof = self.move_to_next_line();
            if not_eof {
                is_current_line_blank = self.is_current_line_blank();
            }
        }

        if not_eof {
            block_lines.push(String::new());
            self.move_to_previous_line();
        } else if !self.is_current_line_last_line_in_document() {
            block_lines.push(String::new());
        }

        let mut text;
        if style == ">" {
            text = String::new();
            let mut previous_line_indented: Option<bool> = None;
            let mut previous_line_blank = false;
            for (i, line) in block_lines.iter().enumerate() {
                if line.is_empty() {
                    text.push('\n');
                    previous_line_blank = true;
                } else if line.starts_with(' ') || line.starts_with('\t') {
                    if previous_line_indented.is_some() {
                        text.push('\n');
                    }
                    text.push_str(line);
                    previous_line_indented = Some(true);
                    previous_line_blank = false;
                } else if previous_line_indented == Some(true) {
                    text.push('\n');
                    text.push_str(line);
                    previous_line_indented = Some(false);
                    previous_line_blank = false;
                } else if previous_line_blank || i == 0 {
                    text.push_str(line);
                    previous_line_indented = Some(false);
                    previous_line_blank = false;
                } else {
                    text.push(' ');
                    text.push_str(line);
                    previous_line_indented = Some(false);
                    previous_line_blank = false;
                }
            }
        } else {
            text = block_lines.join("\n");
        }

        // deal with trailing newlines
        if chomping.is_empty() {
            let trimmed = text.trim_end_matches('\n');
            if trimmed.len() != text.len() {
                text = format!("{trimmed}\n");
            }
        } else if chomping == "-" {
            text = text.trim_end_matches('\n').to_owned();
        }
        text
    }

    fn is_next_line_indented(&mut self) -> bool {
        let current_indentation = self.current_line_indentation();
        let mut movements = 0;
        let mut eof;
        loop {
            eof = !self.move_to_next_line();
            if !eof {
                movements += 1;
            }
            if eof || !(self.is_current_line_empty() || self.is_current_line_comment()) {
                break;
            }
        }
        if eof {
            for _ in 0..movements {
                self.move_to_previous_line();
            }
            return false;
        }
        let ret = self.current_line_indentation() > current_indentation;
        for _ in 0..movements {
            self.move_to_previous_line();
        }
        ret
    }

    fn is_current_line_empty(&self) -> bool {
        self.is_current_line_blank() || self.is_current_line_comment()
    }

    fn is_current_line_blank(&self) -> bool {
        self.current_line.trim_matches(' ').is_empty()
    }

    fn is_current_line_comment(&self) -> bool {
        let l = if self.current_line.starts_with(' ') {
            self.current_line.trim_start_matches(' ')
        } else {
            self.current_line.as_str()
        };
        l.starts_with('#')
    }

    fn is_current_line_last_line_in_document(&self) -> bool {
        (self.offset as isize + self.current_line_nb)
            >= (self.total_number_of_lines.unwrap_or(0) as isize - 1)
    }

    /// `cleanup()`: newlines, the YAML header and leading comments; at the top level also the
    /// document markers and the root node properties.
    fn cleanup(&mut self, value: &str) -> Result<(String, bool)> {
        let value = value.replace("\r\n", "\n").replace('\r', "\n");
        let value = self.remove_leading_lines(&value, strip_header);
        if self.total_number_of_lines.is_some() {
            return Ok((value, false));
        }
        let (value, count) = strip_document_start(&value);
        let value = strip_root_anchor(&value);
        let offset = self.offset;
        let value = self.remove_leading_lines(&value, strip_blank_lines);
        let has_content_on_start_marker_line = count && offset == self.offset;
        let value = strip_document_end(&value);
        let mut pos = 0;
        for line in value.split('\n') {
            if let Some(rest) = line
                .strip_prefix("---")
                .or_else(|| line.strip_prefix("..."))
                && (rest.is_empty() || rest.starts_with([' ', '\t']))
            {
                return Err(ParseError::new(
                    "Multiple documents are not supported.",
                    self.offset as i64 + value[..pos].matches('\n').count() as i64 + 1,
                    Some(line),
                ));
            }
            pos += line.len() + 1;
        }
        Ok((value, has_content_on_start_marker_line))
    }

    fn remove_leading_lines(&mut self, value: &str, strip: fn(&str) -> &str) -> String {
        let trimmed = strip(value);
        self.offset += value.matches('\n').count() - trimmed.matches('\n').count();
        trimmed.to_owned()
    }

    fn is_next_line_in_block_scalar(&self, value: &str) -> bool {
        let next = (self.current_line_nb + 1) as usize;
        if next >= self.lines.len() || !is_match(&NEXT_LINE_BLOCK_SCALAR, value) {
            return false;
        }
        let next_line = &self.lines[next];
        next_line.trim_matches(' ').is_empty()
            || strspn(next_line, b" ", 0) > self.current_line_indentation()
    }

    fn is_next_line_unindented_collection(&mut self) -> bool {
        let current_indentation = self.current_line_indentation();
        let mut movements = 0;
        let mut eof;
        loop {
            eof = !self.move_to_next_line();
            if !eof {
                movements += 1;
            }
            if eof || !(self.is_current_line_empty() || self.is_current_line_comment()) {
                break;
            }
        }
        let ret = !eof
            && self.current_line_indentation() == current_indentation
            && self.is_string_unindented_collection_item();
        for _ in 0..movements {
            self.move_to_previous_line();
        }
        ret
    }

    fn is_string_unindented_collection_item(&self) -> bool {
        rtrim(&self.current_line) == "-" || self.current_line.starts_with("- ")
    }

    /// `getLineTag()`: custom tags are not enabled, so every tag except `!!str`/`!!binary` on an
    /// inline scalar is an error.
    fn get_line_tag(&mut self, value: &str, next_line_check: bool) -> Result<Option<String>> {
        if !value.starts_with('!') {
            return Ok(None);
        }
        let Some(c) = captures(&LINE_TAG, value) else {
            return Ok(None);
        };
        let full_tag = c
            .name("tag")
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default();
        if next_line_check
            && !self.is_next_line_indented()
            && (self.is_string_unindented_collection_item()
                || !self.is_next_line_unindented_collection())
        {
            return Ok(None);
        }
        let tag = substr_from(&full_tag, 1);
        if tag.starts_with('!') {
            if !next_line_check
                && (tag == "!str" || tag == "!binary")
                && !self.is_next_line_indented()
            {
                return Ok(None);
            }
            return Err(ParseError::new(
                format!("The built-in tag \"!{tag}\" is not implemented."),
                self.real_line() + 1,
                Some(value),
            ));
        }
        Err(ParseError::new(
            format!(
                "Tags support is not enabled. You must use the flag \"Yaml::PARSE_CUSTOM_TAGS\" to use \"{full_tag}\"."
            ),
            self.real_line() + 1,
            Some(value),
        ))
    }

    fn lex_inline_quoted_string(&mut self, cursor: &mut usize) -> Result<String> {
        let quotation = self.cur(*cursor).unwrap_or(b'"');
        let mut value: Vec<u8> = vec![quotation];
        *cursor += 1;
        let mut previous_line_was_newline = true;
        let mut previous_line_was_terminated_with_backslash = false;
        let mut line_number = 0;
        loop {
            line_number += 1;
            if line_number > 1 {
                *cursor += strspn(&self.current_line, b" ", *cursor);
            }
            if self.is_current_line_blank() {
                value.push(b'\n');
            } else if !previous_line_was_newline && !previous_line_was_terminated_with_backslash {
                value.push(b' ');
            }
            let line = self.current_line.as_bytes().to_vec();
            while line.len() > *cursor {
                let c = line[*cursor];
                if c == b'\\' {
                    if quotation == b'\'' {
                        value.push(b'\\');
                    } else {
                        *cursor += 1;
                        if let Some(&n) = line.get(*cursor) {
                            value.push(b'\\');
                            value.push(n);
                        }
                    }
                } else if c == quotation {
                    *cursor += 1;
                    if quotation == b'\'' && line.get(*cursor) == Some(&b'\'') {
                        value.extend_from_slice(b"''");
                    } else {
                        value.push(quotation);
                        return Ok(String::from_utf8_lossy(&value).into_owned());
                    }
                } else {
                    value.push(c);
                }
                *cursor += 1;
            }
            if self.is_current_line_blank() {
                previous_line_was_newline = true;
                previous_line_was_terminated_with_backslash = false;
            } else if quotation == b'"'
                && (self.current_line.len() - self.current_line.trim_end_matches('\\').len()) % 2
                    == 1
            {
                previous_line_was_newline = false;
                previous_line_was_terminated_with_backslash = true;
            } else {
                previous_line_was_newline = false;
                previous_line_was_terminated_with_backslash = false;
            }
            if self.has_more_lines() {
                *cursor = 0;
            }
            if !self.move_to_next_line() {
                break;
            }
        }
        Err(ParseError::bare("Malformed inline YAML string."))
    }

    fn lex_unquoted_string(&self, cursor: &mut usize) -> Result<String> {
        let offset = *cursor;
        let line = self.current_line.as_bytes();
        while *cursor < line.len() {
            let c = line[*cursor];
            if matches!(c, b'[' | b']' | b'{' | b'}' | b',' | b':') {
                break;
            }
            if matches!(c, b' ' | b'\t') && line.get(*cursor + 1) == Some(&b'#') {
                break;
            }
            *cursor += 1;
        }
        if *cursor == offset {
            return Err(ParseError::bare("Malformed unquoted YAML string."));
        }
        Ok(substr(&self.current_line, offset, *cursor - offset))
    }

    fn lex_inline_anchor_or_alias(&self, cursor: &mut usize) -> String {
        let offset = *cursor;
        *cursor += 1;
        let line = self.current_line.as_bytes();
        while *cursor < line.len()
            && !matches!(
                line[*cursor],
                b' ' | b'\t' | b',' | b'[' | b']' | b'{' | b'}'
            )
        {
            *cursor += 1;
        }
        substr(&self.current_line, offset, *cursor - offset)
    }

    fn lex_inline_structure(
        &mut self,
        ctx: &mut Ctx,
        cursor: &mut usize,
        closing: u8,
        consume_until_eol: bool,
    ) -> Result<String> {
        ctx.enter(self.real_line() + 1, Some(&self.current_line.clone()))?;
        let r = self.do_lex_inline_structure(ctx, cursor, closing, consume_until_eol);
        ctx.leave();
        r
    }

    fn do_lex_inline_structure(
        &mut self,
        ctx: &mut Ctx,
        cursor: &mut usize,
        closing: u8,
        consume_until_eol: bool,
    ) -> Result<String> {
        let mut value = String::from(self.cur(*cursor).map_or(' ', char::from));
        *cursor += 1;
        loop {
            self.consume_whitespaces(cursor);
            'line: while let Some(c) = self.cur(*cursor) {
                match c {
                    b'"' | b'\'' => {
                        let s = self.lex_inline_quoted_string(cursor)?;
                        value.push_str(&s);
                    }
                    b':' | b',' => {
                        value.push(char::from(c));
                        *cursor += 1;
                    }
                    b'{' => {
                        let s = self.lex_inline_structure(ctx, cursor, b'}', false)?;
                        value.push_str(&s);
                    }
                    b'[' => {
                        let s = self.lex_inline_structure(ctx, cursor, b']', false)?;
                        value.push_str(&s);
                    }
                    b'&' | b'*' => {
                        let s = self.lex_inline_anchor_or_alias(cursor);
                        value.push_str(&s);
                    }
                    c if c == closing => {
                        value.push(char::from(c));
                        *cursor += 1;
                        if consume_until_eol && *cursor < self.current_line.len() {
                            let whitespaces = strspn(&self.current_line, b" ", *cursor) + *cursor;
                            if whitespaces < self.current_line.len()
                                && self.cur(whitespaces) != Some(b'#')
                            {
                                return Err(ParseError::bare(format!(
                                    "Unexpected token \"{}\".",
                                    trim(&substr_from(&self.current_line, *cursor))
                                )));
                            }
                        }
                        return Ok(value);
                    }
                    b'#' => break 'line,
                    _ => {
                        let s = self.lex_unquoted_string(cursor)?;
                        value.push_str(&s);
                    }
                }
                if self.consume_whitespaces(cursor) {
                    value.push(' ');
                }
            }
            if self.has_more_lines() {
                *cursor = 0;
            }
            if !self.move_to_next_line() {
                break;
            }
        }
        Err(ParseError::bare("Malformed inline YAML string."))
    }

    fn consume_whitespaces(&mut self, cursor: &mut usize) -> bool {
        let mut consumed = 0;
        loop {
            let n = strspn(&self.current_line, b" \t", *cursor);
            consumed += n;
            *cursor += n;
            if *cursor < self.current_line.len() {
                return consumed > 0;
            }
            if self.has_more_lines() {
                *cursor = 0;
            }
            if !self.move_to_next_line() {
                break;
            }
        }
        consumed > 0
    }
}

fn unreachable_tag() {
    // `getLineTag()` returns a tag only with `Yaml::PARSE_CUSTOM_TAGS`, which upstream never passes.
}

fn duplicate_key(key: &Key, line: i64, snippet: &str) -> ParseError {
    let k = match key {
        Key::Int(i) => i.to_string(),
        Key::Str(s) => s.clone(),
    };
    ParseError::new(
        format!("Duplicate key \"{k}\" detected."),
        line,
        Some(snippet),
    )
}

/// `isset($data[$key])`: the key exists and its value is not null.
fn isset(data: &[(Key, PhpValue)], key: &Key) -> bool {
    data.iter().any(|(k, v)| k == key && *v != PhpValue::Null)
}

fn array_key_exists(data: &[(Key, PhpValue)], key: &Key) -> bool {
    data.iter().any(|(k, _)| k == key)
}

/// `$data[$key] = $value`.
fn set_key(data: &mut Vec<(Key, PhpValue)>, key: Key, value: PhpValue) {
    match data.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => data.push((key, value)),
    }
}

/// `$data += $other` (array union: keys that are not set yet).
fn array_union(data: &mut Vec<(Key, PhpValue)>, other: Vec<(Key, PhpValue)>) {
    for (k, v) in other {
        if !data.iter().any(|(existing, _)| *existing == k) {
            data.push((k, v));
        }
    }
}

/// `trimTag()`.
fn trim_tag(value: &str) -> String {
    if value.starts_with('!') {
        let len = strcspn(value, b" \r\n", 1);
        return substr(value, 1, len).trim_start_matches(' ').to_owned();
    }
    value.to_owned()
}

/// `moveAnchorBeforeTag()`.
fn move_anchor_before_tag(value: &str) -> String {
    if value.starts_with("!!") {
        return value.to_owned();
    }
    ANCHOR_AFTER_TAG.replace(value, "$2 $1").into_owned()
}

/// `(int) $modifiers`: the leading integer of a string.
fn php_int_prefix(s: &str) -> i64 {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0B', '\x0C']);
    let mut end = 0;
    let b = t.as_bytes();
    if matches!(b.first(), Some(b'+' | b'-')) {
        end = 1;
    }
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    t[..end].parse().unwrap_or(0)
}

/// Lines that are blank or comments, then an optional `%YAML` directive with its trailing blank
/// and comment lines (the first pattern of `cleanup()`).
fn strip_header(value: &str) -> &str {
    let mut rest = strip_blank_or_comment_lines(value);
    if let Some(after) = rest.strip_prefix("%YAML")
        && after.starts_with([':', ' '])
    {
        let after = &after[1..];
        let digits = after
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'.')
            .count();
        if digits > 0
            && let Some(nl) = after.find('\n')
        {
            rest = strip_blank_or_comment_lines(&after[nl + 1..]);
        }
    }
    rest
}

/// `#^(?:[ \t]*+(?:\#[^\n]*+)?+\n)++#`.
fn strip_blank_lines(value: &str) -> &str {
    strip_blank_or_comment_lines(value)
}

fn strip_blank_or_comment_lines(value: &str) -> &str {
    let mut rest = value;
    loop {
        let b = rest.as_bytes();
        let mut i = 0;
        while i < b.len() && matches!(b[i], b' ' | b'\t') {
            i += 1;
        }
        if i < b.len() && b[i] == b'#' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        }
        if i < b.len() && b[i] == b'\n' {
            rest = &rest[i + 1..];
        } else {
            return rest;
        }
    }
}

/// Removes the document start marker (`---`) with what may follow it on its line. Returns whether
/// it was removed.
fn strip_document_start(value: &str) -> (String, bool) {
    let Some(rest) = value.strip_prefix("---") else {
        return (value.to_owned(), false);
    };
    let b = rest.as_bytes();
    if b.is_empty() || b[0] == b'\n' {
        return (rest.to_owned(), true);
    }
    if !matches!(b[0], b' ' | b'\t') {
        return (value.to_owned(), false);
    }
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    let ws_end = i;
    if b.get(i) == Some(&b'%') {
        while i < b.len() && b[i] != b'\n' {
            i += 1;
        }
        return (rest[i..].to_owned(), true);
    }
    // `(?:(?:[!&][^ \t\n]*+|[|>][-+0-9]*+)[ \t]*+)++` followed by a comment, a newline or the end.
    let mut j = i;
    let mut matched_any = false;
    loop {
        match b.get(j) {
            Some(b'!' | b'&') => {
                j += 1;
                while j < b.len() && !matches!(b[j], b' ' | b'\t' | b'\n') {
                    j += 1;
                }
            }
            Some(b'|' | b'>') => {
                j += 1;
                while j < b.len() && matches!(b[j], b'-' | b'+' | b'0'..=b'9') {
                    j += 1;
                }
            }
            _ => break,
        }
        matched_any = true;
        while j < b.len() && matches!(b[j], b' ' | b'\t') {
            j += 1;
        }
    }
    if matched_any {
        let mut k = j;
        if b.get(k) == Some(&b'#') {
            while k < b.len() && b[k] != b'\n' {
                k += 1;
            }
        }
        if k == b.len() || b[k] == b'\n' {
            return (rest[j..].to_owned(), true);
        }
    }
    (rest[ws_end..].to_owned(), true)
}

/// `#^((?:![^! \t\n][^ \t\n]*+[ \t]++)?+)&[^ \t\n]++(?:[ \t]++|(?=\n)|\z)#` → `$1`.
fn strip_root_anchor(value: &str) -> String {
    let b = value.as_bytes();
    let mut i = 0;
    // optional tag
    if b.first() == Some(&b'!')
        && b.get(1)
            .is_some_and(|c| !matches!(c, b'!' | b' ' | b'\t' | b'\n'))
    {
        let mut j = 2;
        while j < b.len() && !matches!(b[j], b' ' | b'\t' | b'\n') {
            j += 1;
        }
        let ws = j;
        while j < b.len() && matches!(b[j], b' ' | b'\t') {
            j += 1;
        }
        if j > ws {
            i = j;
        }
    }
    let tag_end = i;
    if b.get(i) != Some(&b'&') {
        return value.to_owned();
    }
    let mut j = i + 1;
    while j < b.len() && !matches!(b[j], b' ' | b'\t' | b'\n') {
        j += 1;
    }
    if j == i + 1 {
        return value.to_owned();
    }
    if j < b.len() && matches!(b[j], b' ' | b'\t') {
        while j < b.len() && matches!(b[j], b' ' | b'\t') {
            j += 1;
        }
    }
    format!("{}{}", &value[..tag_end], &value[j..])
}

/// `#^\.\.\.(?:[ \t]++(?:\#[^\n]*+)?+)?+(?:\n[ \t]*+(?:\#[^\n]*+)?+)*+\z#m` → ''.
fn strip_document_end(value: &str) -> String {
    let mut pos = 0;
    while pos <= value.len() {
        let rest = &value[pos..];
        if rest.starts_with("...") && trailer_is_comments(&rest[3..]) {
            return value[..pos].to_owned();
        }
        match rest.find('\n') {
            Some(nl) => pos += nl + 1,
            None => break,
        }
    }
    value.to_owned()
}

fn trailer_is_comments(rest: &str) -> bool {
    let b = rest.as_bytes();
    let mut i = 0;
    if i < b.len() && matches!(b[i], b' ' | b'\t') {
        while i < b.len() && matches!(b[i], b' ' | b'\t') {
            i += 1;
        }
        if b.get(i) == Some(&b'#') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        }
    }
    loop {
        if i == b.len() {
            return true;
        }
        if b[i] != b'\n' {
            return false;
        }
        i += 1;
        while i < b.len() && matches!(b[i], b' ' | b'\t') {
            i += 1;
        }
        if b.get(i) == Some(&b'#') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Inline

fn inline_parse(ctx: &mut Ctx, value: &str) -> Result<PhpValue> {
    let value = trim(value);
    if value.is_empty() {
        return Ok(PhpValue::Str(String::new()));
    }
    let mut i = 0;
    let tag = parse_tag(ctx, value, &mut i)?;
    if let Some(t) = tag.as_deref()
        && !t.is_empty()
        && (i >= value.len() || byte_at(value, i) == Some(b'#'))
    {
        // A custom tag without value: only reachable with PARSE_CUSTOM_TAGS.
        return Ok(PhpValue::Str(String::new()));
    }
    let result = match byte_at(value, i) {
        Some(b'[') => {
            let r = parse_sequence(ctx, value, &mut i)?;
            i += 1;
            r
        }
        Some(b'{') => {
            let r = parse_mapping(ctx, value, &mut i)?;
            i += 1;
            r
        }
        _ => {
            let mut quoted = false;
            parse_scalar(ctx, value, None, &mut i, true, &mut quoted)?
        }
    };
    // some comments are allowed at the end
    let rest = substr_from(value, i);
    if php_truthy(&leftover_after_comment(&rest)) {
        return Err(ParseError::new(
            format!("Unexpected characters near \"{rest}\"."),
            ctx.parsed_line_number + 1,
            Some(value),
        ));
    }
    Ok(result)
}

/// `Inline::parseScalar()`.
fn parse_scalar(
    ctx: &mut Ctx,
    scalar: &str,
    delimiters: Option<&[&str]>,
    i: &mut usize,
    evaluate: bool,
    is_quoted: &mut bool,
) -> Result<PhpValue> {
    if matches!(byte_at(scalar, *i), Some(b'"' | b'\'')) {
        *is_quoted = true;
        let output = parse_quoted_scalar(ctx, scalar, i)?;
        if let Some(delims) = delimiters {
            let tmp = substr_from(scalar, *i);
            let tmp = tmp.trim_start_matches([' ', '\n']);
            if tmp.is_empty() {
                return Err(ParseError::new(
                    format!(
                        "Unexpected end of line, expected one of \"{}\".",
                        delims.concat()
                    ),
                    ctx.parsed_line_number + 1,
                    Some(scalar),
                ));
            }
            if !delims.iter().any(|d| tmp.starts_with(d)) {
                return Err(ParseError::new(
                    format!("Unexpected characters ({}).", substr_from(scalar, *i)),
                    ctx.parsed_line_number + 1,
                    Some(scalar),
                ));
            }
        }
        return Ok(PhpValue::Str(output));
    }
    *is_quoted = false;
    let mut output;
    match delimiters {
        None => {
            output = substr_from(scalar, *i);
            *i += output.len();
            if let Some(pos) = comment_start(&output) {
                output.truncate(pos);
            }
        }
        Some(delims) => {
            let rest = substr_from(scalar, *i);
            let b = rest.as_bytes();
            let mut found = None;
            for p in 0..=b.len() {
                if delims.iter().any(|d| b[p..].starts_with(d.as_bytes())) {
                    found = Some(p);
                    break;
                }
                if p < b.len() && b[p] == b'\n' {
                    break;
                }
            }
            let Some(p) = found else {
                return Err(ParseError::new(
                    format!("Malformed inline YAML string: \"{scalar}\"."),
                    ctx.parsed_line_number + 1,
                    None,
                ));
            };
            output = substr(&rest, 0, p);
            *i += output.len();
            output = trim(&output).to_owned();
        }
    }
    // a non-quoted string cannot start with @ or ` (reserved) nor with a scalar indicator (| or >)
    if php_truthy(&output)
        && let Some(c) = byte_at(&output, 0)
        && matches!(c, b'@' | b'`' | b'|' | b'>' | b'%')
    {
        return Err(ParseError::new(
            format!(
                "The reserved indicator \"{}\" cannot start a plain scalar; you need to quote the scalar.",
                char::from(c)
            ),
            ctx.parsed_line_number + 1,
            Some(&output),
        ));
    }
    if evaluate {
        let mut q = false;
        return evaluate_scalar(ctx, &output, &mut q);
    }
    Ok(PhpValue::Str(output))
}

/// `Inline::parseQuotedScalar()`.
fn parse_quoted_scalar(ctx: &Ctx, scalar: &str, i: &mut usize) -> Result<String> {
    let rest = &scalar.as_bytes()[*i..];
    let quote = rest[0];
    let mut j = 1;
    let matched = if quote == b'"' {
        loop {
            match rest.get(j) {
                None => break None,
                Some(b'"') => break Some(j + 1),
                Some(b'\\') => match rest.get(j + 1) {
                    None | Some(b'\n') => break None,
                    Some(_) => {
                        // `.` is one UTF-8 character
                        let ch_len = utf8_len(rest[j + 1]);
                        j += 1 + ch_len;
                    }
                },
                Some(_) => j += 1,
            }
        }
    } else {
        loop {
            match rest.get(j) {
                None => break None,
                Some(b'\'') => {
                    if rest.get(j + 1) == Some(&b'\'') {
                        j += 2;
                    } else {
                        break Some(j + 1);
                    }
                }
                Some(_) => j += 1,
            }
        }
    };
    let Some(len) = matched else {
        return Err(ParseError::new(
            format!(
                "Malformed inline YAML string: \"{}\".",
                substr_from(scalar, *i)
            ),
            ctx.parsed_line_number + 1,
            Some(scalar),
        ));
    };
    let inner = String::from_utf8_lossy(&rest[1..len - 1]).into_owned();
    let out = if quote == b'"' {
        unescape_double_quoted(&inner)?
    } else {
        inner.replace("''", "'")
    };
    *i += len;
    Ok(out)
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// `Unescaper::unescapeDoubleQuotedString()`.
fn unescape_double_quoted(value: &str) -> Result<String> {
    let b = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex_run = |from: usize, n: usize| -> bool {
        b.len() >= from + n && b[from..from + n].iter().all(u8::is_ascii_hexdigit)
    };
    while i < b.len() {
        if b[i] != b'\\' || i + 1 >= b.len() || b[i + 1] == b'\n' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let c = b[i + 1];
        let (matched_len, digits) = match c {
            b'x' if hex_run(i + 2, 2) => (4, Some(2)),
            b'u' if hex_run(i + 2, 4) => (6, Some(4)),
            b'U' if hex_run(i + 2, 8) => (10, Some(8)),
            _ => (1 + utf8_len(c), None),
        };
        let escaped: Vec<u8> = match c {
            b'0' => vec![0],
            b'a' => vec![0x07],
            b'b' => vec![0x08],
            b't' | b'\t' => vec![b'\t'],
            b'n' => vec![b'\n'],
            b'v' => vec![0x0b],
            b'f' => vec![0x0c],
            b'r' => vec![b'\r'],
            b'e' => vec![0x1b],
            b' ' => vec![b' '],
            b'"' => vec![b'"'],
            b'/' => vec![b'/'],
            b'\\' => vec![b'\\'],
            b'N' => "\u{85}".as_bytes().to_vec(),
            b'_' => "\u{a0}".as_bytes().to_vec(),
            b'L' => "\u{2028}".as_bytes().to_vec(),
            b'P' => "\u{2029}".as_bytes().to_vec(),
            b'x' | b'u' | b'U' => {
                let code = digits.map_or(0, |n| {
                    u32::from_str_radix(&value[i + 2..i + 2 + n], 16).unwrap_or(0)
                });
                utf8chr(code)
            }
            _ => {
                let seq =
                    String::from_utf8_lossy(&b[i..(i + matched_len).min(b.len())]).into_owned();
                return Err(ParseError::bare(format!(
                    "Found unknown escape character \"{seq}\"."
                )));
            }
        };
        out.extend(escaped);
        i += matched_len;
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// `Unescaper::utf8chr()`.
fn utf8chr(code: u32) -> Vec<u8> {
    let c = code % 0x200000;
    match char::from_u32(c) {
        Some(ch) => ch.to_string().into_bytes(),
        None => "\u{fffd}".as_bytes().to_vec(),
    }
}

/// `Inline::parseSequence()`.
fn parse_sequence(ctx: &mut Ctx, sequence: &str, i: &mut usize) -> Result<PhpValue> {
    ctx.enter(ctx.parsed_line_number + 1, None)?;
    let r = do_parse_sequence(ctx, sequence, i);
    ctx.leave();
    r
}

fn do_parse_sequence(ctx: &mut Ctx, sequence: &str, i: &mut usize) -> Result<PhpValue> {
    let mut output = PhpValue::Array(Vec::new());
    let b = sequence.as_bytes();
    let len = b.len();
    *i += 1;
    let mut last_token: Option<&str> = None;
    while *i < len {
        if b[*i] == b']' {
            return Ok(output);
        }
        if b[*i] == b',' || b[*i] == b' ' {
            if b[*i] == b',' && last_token.is_none_or(|t| t == "separator") {
                output.push(PhpValue::Null);
            } else if b[*i] == b',' {
                last_token = Some("separator");
            }
            *i += 1;
            continue;
        }
        let mut tag = parse_tag(ctx, sequence, i)?;
        let anchor = parse_anchor(sequence, i);
        if anchor.is_some() && tag.is_none() {
            tag = parse_tag(ctx, sequence, i)?;
        }
        if let Some(a) = anchor.clone()
            && (*i >= len || b[*i] == b',' || b[*i] == b']')
        {
            ctx.refs.insert(a, PhpValue::Null);
            output.push(PhpValue::Null);
            last_token = Some("value");
            continue;
        }
        if *i >= len {
            break;
        }
        let value = match b[*i] {
            b'[' => parse_sequence(ctx, sequence, i)?,
            b'{' => parse_mapping(ctx, sequence, i)?,
            _ => {
                let mut is_quoted = false;
                let mut value = parse_scalar(
                    ctx,
                    sequence,
                    Some(&[",", "]"]),
                    i,
                    tag.is_none(),
                    &mut is_quoted,
                )?;
                // the value can be an array if a reference has been resolved to an array var
                if let PhpValue::Str(s) = &value
                    && !is_quoted
                    && s.contains(": ")
                {
                    // embedded mapping?
                    let mut j = *i;
                    let mut mapping_value = s.clone();
                    let mut mapping_exception: Option<ParseError>;
                    loop {
                        let mut pos = 0;
                        match parse_mapping(ctx, &format!("{{{mapping_value}}}"), &mut pos) {
                            Ok(v) => {
                                value = v;
                                *i = j;
                                mapping_exception = None;
                                break;
                            }
                            Err(e) => {
                                mapping_exception = Some(e);
                                if j >= len {
                                    break;
                                }
                                mapping_value.push(char::from(b[j]));
                                j += 1;
                                if j >= len {
                                    break;
                                }
                                let mut q = false;
                                let next = parse_scalar(
                                    ctx,
                                    sequence,
                                    Some(&[",", "]"]),
                                    &mut j,
                                    tag.is_none(),
                                    &mut q,
                                )?;
                                mapping_value.push_str(&php_to_string(&next));
                            }
                        }
                        if j >= len {
                            break;
                        }
                    }
                    if let Some(e) = mapping_exception {
                        return Err(e);
                    }
                }
                *i -= 1;
                value
            }
        };
        if let Some(a) = anchor {
            ctx.refs.insert(a, value.clone());
        }
        output.push(value);
        last_token = Some("value");
        *i += 1;
    }
    Err(ParseError::new(
        format!("Malformed inline YAML string: \"{sequence}\"."),
        ctx.parsed_line_number + 1,
        None,
    ))
}

/// PHP string conversion of a scalar (`$x .= $value`).
fn php_to_string(v: &PhpValue) -> String {
    match v {
        PhpValue::Null | PhpValue::Bool(false) => String::new(),
        PhpValue::Bool(true) => "1".into(),
        PhpValue::Int(i) => i.to_string(),
        PhpValue::Float(f) => format!("{f}"),
        PhpValue::Str(s) => s.clone(),
        PhpValue::Array(_) => "Array".into(),
    }
}

/// `Inline::parseMapping()`.
fn parse_mapping(ctx: &mut Ctx, mapping: &str, i: &mut usize) -> Result<PhpValue> {
    ctx.enter(ctx.parsed_line_number + 1, None)?;
    let r = do_parse_mapping(ctx, mapping, i);
    ctx.leave();
    r
}

#[allow(clippy::too_many_lines)]
fn do_parse_mapping(ctx: &mut Ctx, mapping: &str, i: &mut usize) -> Result<PhpValue> {
    let mut output: Vec<(Key, PhpValue)> = Vec::new();
    let b = mapping.as_bytes();
    let len = b.len();
    *i += 1;
    let mut allow_overwrite = false;
    'outer: while *i < len {
        match b[*i] {
            b' ' | b',' | b'\n' => {
                *i += 1;
                continue;
            }
            b'}' => return Ok(PhpValue::Array(output)),
            _ => {}
        }
        // key
        let offset_before_key_parsing = *i;
        let is_key_quoted = matches!(b[*i], b'"' | b'\'');
        let mut q = false;
        let key = parse_scalar(ctx, mapping, Some(&[":", " "]), i, false, &mut q)?;
        let key = php_to_string(&key);
        if offset_before_key_parsing == *i {
            return Err(ParseError::new(
                "Missing mapping key.",
                ctx.parsed_line_number + 1,
                Some(mapping),
            ));
        }
        match mapping[*i..].find(':') {
            Some(p) => *i += p,
            None => break,
        }
        if !is_key_quoted {
            let mut q = false;
            let evaluated = evaluate_scalar(ctx, &key, &mut q)?;
            let same = evaluated == PhpValue::Str(key.clone());
            if !key.is_empty()
                && !same
                && (!matches!(evaluated, PhpValue::Str(_)) || key.starts_with('!'))
                && !matches!(evaluated, PhpValue::Int(_))
            {
                return Err(ParseError::new(
                    "Implicit casting of incompatible mapping keys to strings is not supported. Quote your evaluable mapping keys instead.",
                    ctx.parsed_line_number + 1,
                    Some(mapping),
                ));
            }
        }
        if !is_key_quoted
            && !b
                .get(*i + 1)
                .is_some_and(|c| matches!(c, b' ' | b',' | b'[' | b']' | b'{' | b'}' | b'\n'))
        {
            return Err(ParseError::new(
                "Colons must be followed by a space or an indication character (i.e. \" \", \",\", \"[\", \"]\", \"{\", \"}\").",
                ctx.parsed_line_number + 1,
                Some(mapping),
            ));
        }
        if key == "<<" {
            allow_overwrite = true;
        }
        let k = Key::from_php_string(&key);
        while *i < len {
            if matches!(b[*i], b':' | b' ' | b'\n') {
                *i += 1;
                continue;
            }
            let mut tag = parse_tag(ctx, mapping, i)?;
            let anchor = parse_anchor(mapping, i);
            if anchor.is_some() && tag.is_none() {
                tag = parse_tag(ctx, mapping, i)?;
            }
            if let Some(a) = anchor.clone()
                && (*i >= len || matches!(b[*i], b',' | b'}' | b'\n'))
            {
                ctx.refs.insert(a, PhpValue::Null);
                if key != "<<" {
                    if allow_overwrite || !isset(&output, &k) {
                        set_key(&mut output, k.clone(), PhpValue::Null);
                    } else {
                        return Err(duplicate_key_inline(&key, ctx, mapping));
                    }
                }
                continue 'outer;
            }
            if *i >= len {
                break;
            }
            match b[*i] {
                b'[' => {
                    let value = parse_sequence(ctx, mapping, i)?;
                    if let Some(a) = anchor {
                        ctx.refs.insert(a, value.clone());
                    }
                    if key == "<<" {
                        if let PhpValue::Array(items) = value {
                            for (_, parsed) in items {
                                if let PhpValue::Array(sub) = parsed {
                                    array_union(&mut output, sub);
                                }
                            }
                        }
                    } else if allow_overwrite || !isset(&output, &k) {
                        set_key(&mut output, k.clone(), value);
                    } else {
                        return Err(duplicate_key_inline(&key, ctx, mapping));
                    }
                }
                b'{' => {
                    let value = parse_mapping(ctx, mapping, i)?;
                    if let Some(a) = anchor {
                        ctx.refs.insert(a, value.clone());
                    }
                    if key == "<<" {
                        if let PhpValue::Array(items) = value {
                            array_union(&mut output, items);
                        }
                    } else if allow_overwrite || !isset(&output, &k) {
                        set_key(&mut output, k.clone(), value);
                    } else {
                        return Err(duplicate_key_inline(&key, ctx, mapping));
                    }
                }
                _ => {
                    let mut q = false;
                    let value = parse_scalar(
                        ctx,
                        mapping,
                        Some(&[",", "}", "\n"]),
                        i,
                        tag.is_none(),
                        &mut q,
                    )?;
                    if let Some(a) = anchor {
                        ctx.refs.insert(a, value.clone());
                    }
                    if key == "<<" {
                        if let PhpValue::Array(items) = value {
                            array_union(&mut output, items);
                        }
                    } else if allow_overwrite || !isset(&output, &k) {
                        set_key(&mut output, k.clone(), value);
                    } else {
                        return Err(duplicate_key_inline(&key, ctx, mapping));
                    }
                    *i -= 1;
                }
            }
            *i += 1;
            continue 'outer;
        }
    }
    Err(ParseError::new(
        format!("Malformed inline YAML string: \"{mapping}\"."),
        ctx.parsed_line_number + 1,
        None,
    ))
}

fn duplicate_key_inline(key: &str, ctx: &Ctx, mapping: &str) -> ParseError {
    ParseError::new(
        format!("Duplicate key \"{key}\" detected."),
        ctx.parsed_line_number + 1,
        Some(mapping),
    )
}

static OCTAL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(?:\+|-)?0o(?P<value>[0-7_]+)$").unwrap_or_else(|_| unreachable!())
});
static SIGNED_DIGITS: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[+-]?[0-9][0-9_]*$").unwrap_or_else(|_| unreachable!()));
static HEX: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^0x[0-9a-fA-F_]+$").unwrap_or_else(|_| unreachable!()));
static FLOAT_UNDERSCORE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(-|\+)?[0-9][0-9_]*(\.[0-9_]+)?$").unwrap_or_else(|_| unreachable!())
});
static TIMESTAMP: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"^(?P<year>[0-9][0-9][0-9][0-9])-(?P<month>[0-9][0-9]?)-(?P<day>[0-9][0-9]?)(?:(?:[Tt]|[ \t]+)(?P<hour>[0-9][0-9]?):(?P<minute>[0-9][0-9]):(?P<second>[0-9][0-9])(?:\.(?P<fraction>[0-9]*))?(?:[ \t]*(?P<tz>Z|(?P<tz_sign>[-+])(?P<tz_hour>[0-9][0-9]?)(?::(?P<tz_minute>[0-9][0-9]))?))?)?$",
    )
    .unwrap_or_else(|_| unreachable!())
});

/// `ctype_digit()`.
fn ctype_digit(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `(float) $s` for a numeric string.
fn php_float_value(s: &str) -> f64 {
    let t = s.trim_start_matches([' ', '\t', '\n', '\r', '\x0B', '\x0C']);
    // The longest numeric prefix, as PHP's strtod does.
    let b = t.as_bytes();
    let mut end = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        end = 1;
    }
    let digits_start = end;
    while end < b.len() && b[end].is_ascii_digit() {
        end += 1;
    }
    if end < b.len() && b[end] == b'.' {
        end += 1;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
        }
    }
    if end == digits_start || (end == digits_start + 1 && b.get(digits_start) == Some(&b'.')) {
        return 0.0;
    }
    if end < b.len() && matches!(b[end], b'e' | b'E') {
        let mut e = end + 1;
        if matches!(b.get(e), Some(b'+' | b'-')) {
            e += 1;
        }
        let exp_digits = e;
        while e < b.len() && b[e].is_ascii_digit() {
            e += 1;
        }
        if e > exp_digits {
            end = e;
        }
    }
    t[..end].parse().unwrap_or(0.0)
}

/// `hexdec()` / `octdec()`: ignores other characters, gives a float on overflow.
fn base_dec(s: &str, radix: u32) -> PhpValue {
    let mut acc: i64 = 0;
    let mut facc: f64 = 0.0;
    let mut overflow = false;
    for c in s.chars() {
        let Some(d) = c.to_digit(radix) else {
            continue;
        };
        if !overflow {
            match acc
                .checked_mul(i64::from(radix))
                .and_then(|v| v.checked_add(i64::from(d)))
            {
                Some(v) => acc = v,
                None => {
                    overflow = true;
                    facc = acc as f64 * f64::from(radix) + f64::from(d);
                }
            }
        } else {
            facc = facc * f64::from(radix) + f64::from(d);
        }
    }
    if overflow {
        PhpValue::Float(facc)
    } else {
        PhpValue::Int(acc)
    }
}

/// `Inline::evaluateScalar()`.
#[allow(clippy::too_many_lines)]
fn evaluate_scalar(ctx: &mut Ctx, scalar: &str, is_quoted: &mut bool) -> Result<PhpValue> {
    *is_quoted = false;
    let mut scalar = trim(scalar).to_owned();
    if let Some(rest) = scalar.strip_prefix('*') {
        let mut value = rest.to_owned();
        if let Some(pos) = comment_start(&value) {
            value.truncate(pos);
        }
        if value.is_empty() {
            return Err(ParseError::new(
                "A reference must contain at least one character.",
                ctx.parsed_line_number + 1,
                Some(&value),
            ));
        }
        let Some(found) = ctx.refs.get(&value).cloned() else {
            return Err(ParseError::new(
                format!("Reference \"{value}\" does not exist."),
                ctx.parsed_line_number + 1,
                Some(&value),
            ));
        };
        ctx.count_alias(&found, ctx.parsed_line_number + 1, None)?;
        return Ok(found);
    }
    let lower = scalar.to_ascii_lowercase();
    if lower == "null" || scalar.is_empty() || scalar == "~" {
        return Ok(PhpValue::Null);
    }
    if lower == "true" {
        return Ok(PhpValue::Bool(true));
    }
    if lower == "false" {
        return Ok(PhpValue::Bool(false));
    }
    if scalar.starts_with('!') {
        if scalar == "!!str" || scalar == "!!binary" {
            return Ok(PhpValue::Str(String::new()));
        }
        if let Some(s) = scalar.strip_prefix("!!str ") {
            if matches!(byte_at(s, 0), Some(b'"' | b'\'')) {
                *is_quoted = true;
                let mut j = 0;
                return parse_quoted_scalar(ctx, s, &mut j).map(PhpValue::Str);
            }
            return Ok(PhpValue::Str(s.to_owned()));
        }
        if let Some(s) = scalar.strip_prefix("! ") {
            return Ok(PhpValue::Str(s.to_owned()));
        }
        if scalar.starts_with("!php/object")
            || scalar.starts_with("!php/const")
            || scalar.starts_with("!php/enum")
        {
            return Ok(PhpValue::Null);
        }
        if let Some(s) = scalar.strip_prefix("!!float ") {
            return Ok(PhpValue::Float(php_float_value(s)));
        }
        if let Some(s) = scalar.strip_prefix("!!binary ") {
            return evaluate_binary_scalar(ctx, s).map(PhpValue::Str);
        }
        return Err(ParseError::new(
            format!(
                "The string \"{scalar}\" could not be parsed as it uses an unsupported built-in tag."
            ),
            ctx.parsed_line_number,
            Some(&scalar),
        ));
    }
    if let Some(c) = OCTAL.captures(&scalar) {
        let value = c["value"].replace('_', "");
        let n = base_dec(&value, 8);
        if scalar.starts_with('-') {
            return Ok(match n {
                PhpValue::Int(i) => PhpValue::Int(-i),
                PhpValue::Float(f) => PhpValue::Float(-f),
                other => other,
            });
        }
        return Ok(n);
    }
    let first = scalar.as_bytes()[0];
    if matches!(first, b'+' | b'-' | b'.') || first.is_ascii_digit() {
        if SIGNED_DIGITS.is_match(&scalar) {
            scalar = scalar.replace('_', "");
            if scalar.starts_with('+') {
                scalar = scalar[1..].to_owned();
            }
        }
        if ctype_digit(&scalar) || (scalar.starts_with('-') && ctype_digit(&scalar[1..])) {
            return Ok(match scalar.parse::<i64>() {
                Ok(cast) if cast.to_string() == scalar => PhpValue::Int(cast),
                _ => PhpValue::Str(scalar),
            });
        }
        if php_is_numeric(&scalar) || HEX.is_match(&scalar) {
            let stripped = scalar.replace('_', "");
            if stripped.starts_with("0x") {
                return Ok(base_dec(&stripped, 16));
            }
            return Ok(PhpValue::Float(php_float_value(&stripped)));
        }
        if lower == ".inf" {
            return Ok(PhpValue::Float(f64::INFINITY));
        }
        if lower == ".nan" {
            return Ok(PhpValue::Float(f64::NAN));
        }
        if lower == "-.inf" {
            return Ok(PhpValue::Float(f64::NEG_INFINITY));
        }
        if FLOAT_UNDERSCORE.is_match(&scalar) {
            return Ok(PhpValue::Float(php_float_value(&scalar.replace('_', ""))));
        }
        if let Some(c) = TIMESTAMP.captures(&scalar) {
            return timestamp(ctx, &scalar, &c);
        }
    }
    Ok(PhpValue::Str(scalar))
}

/// A YAML timestamp as `new \DateTimeImmutable($scalar, UTC)` gives it: the unix timestamp, or a
/// float with the fraction.
fn timestamp(ctx: &Ctx, scalar: &str, c: &regex::Captures<'_>) -> Result<PhpValue> {
    let num = |name: &str| -> i64 { c.name(name).map_or(0, |m| m.as_str().parse().unwrap_or(0)) };
    let (y, m, d) = (num("year"), num("month"), num("day"));
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(ParseError::new(
            format!("The date \"{scalar}\" could not be parsed as it is an invalid date."),
            ctx.parsed_line_number + 1,
            Some(scalar),
        ));
    }
    let days = days_from_civil(y, m, 1) + (d - 1);
    let mut secs = days * 86_400 + num("hour") * 3_600 + num("minute") * 60 + num("second");
    if c.name("tz").is_some_and(|t| t.as_str() != "Z") {
        let offset = num("tz_hour") * 3_600 + num("tz_minute") * 60;
        if c.name("tz_sign").map(|s| s.as_str()) == Some("-") {
            secs += offset;
        } else {
            secs -= offset;
        }
    }
    let fraction = c.name("fraction").map_or("", |f| f.as_str());
    // DateTime keeps microseconds (6 digits).
    let micros: String = fraction.chars().take(6).collect();
    if !micros.trim_end_matches('0').is_empty() {
        let padded = format!("{micros:0<6}");
        return Ok(PhpValue::Float(php_float_value(&format!(
            "{secs}.{padded}"
        ))));
    }
    Ok(PhpValue::Int(secs))
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `Inline::parseAnchor()`.
fn parse_anchor(value: &str, i: &mut usize) -> Option<String> {
    let b = value.as_bytes();
    if b.get(*i) != Some(&b'&') {
        return None;
    }
    let mut j = *i + 1;
    while j < b.len()
        && !matches!(
            b[j],
            b' ' | b'\t' | b',' | b'[' | b']' | b'{' | b'}' | b'\n'
        )
    {
        j += 1;
    }
    if j == *i + 1 {
        return None;
    }
    let name = substr(value, *i + 1, j - *i - 1);
    while j < b.len() && is_pcre_space(b[j]) {
        j += 1;
    }
    *i = j;
    Some(name)
}

/// `Inline::parseTag()` without custom tag support.
fn parse_tag(ctx: &Ctx, value: &str, i: &mut usize) -> Result<Option<String>> {
    if byte_at(value, *i) != Some(b'!') {
        return Ok(None);
    }
    let tag_length = strcspn(value, b" \t\n[]{},", *i + 1);
    let tag = substr(value, *i + 1, tag_length);
    let mut next_offset = *i + tag_length + 1;
    next_offset += strspn(value, b" ", next_offset);
    let next = byte_at(value, next_offset);
    if tag.is_empty() && next.is_none_or(|c| matches!(c, b']' | b'}' | b',')) {
        return Err(ParseError::new(
            "Using the unquoted scalar value \"!\" is not supported. You must quote it.",
            ctx.parsed_line_number + 1,
            Some(value),
        ));
    }
    // Is followed by a scalar and is a built-in tag
    if !tag.is_empty()
        && next.is_none_or(|c| !matches!(c, b'[' | b'{'))
        && (tag.starts_with('!')
            || ["str", "php/const", "php/enum", "php/object"].contains(&tag.as_str()))
    {
        // Manage in {@link self::evaluateScalar()}
        return Ok(None);
    }
    *i = next_offset;
    // Built-in tags
    if tag.starts_with('!') {
        return Err(ParseError::new(
            format!("The built-in tag \"!{tag}\" is not implemented."),
            ctx.parsed_line_number + 1,
            Some(value),
        ));
    }
    if tag.is_empty() {
        return Ok(Some(tag));
    }
    Err(ParseError::new(
        format!(
            "Tags support is not enabled. Enable the \"Yaml::PARSE_CUSTOM_TAGS\" flag to use \"!{tag}\"."
        ),
        ctx.parsed_line_number + 1,
        Some(value),
    ))
}

/// `Inline::evaluateBinaryScalar()`.
fn evaluate_binary_scalar(ctx: &mut Ctx, scalar: &str) -> Result<String> {
    let data: String = scalar.chars().filter(|c| !c.is_whitespace()).collect();
    if data.is_empty() {
        return Ok(String::new());
    }
    let mut i = 0;
    let mut q = false;
    let parsed = php_to_string(&parse_scalar(ctx, &data, None, &mut i, true, &mut q)?);
    if parsed.is_empty() {
        return Ok(String::new());
    }
    if !parsed.len().is_multiple_of(4) {
        return Err(ParseError::new(
            format!(
                "The normalized base64 encoded data (data without whitespace characters) length must be a multiple of four ({} bytes given).",
                parsed.len()
            ),
            ctx.parsed_line_number + 1,
            Some(scalar),
        ));
    }
    let valid = {
        let body = parsed.trim_end_matches('=');
        parsed.len() - body.len() <= 2
            && !body.is_empty()
            && body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
    };
    if !valid {
        return Err(ParseError::new(
            format!("The base64 encoded data ({parsed}) contains invalid characters."),
            ctx.parsed_line_number + 1,
            Some(scalar),
        ));
    }
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&parsed)
        .unwrap_or_default();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PhpValue {
        parse(s, 256).unwrap_or_else(|e| panic!("{s:?}: {e}"))
    }

    #[test]
    fn scalars_follow_symfony() {
        let doc = p(
            "a: 1_000\nb: 0644\nc: yes\nd: 1.5\ne: ~\nf: '0x1F'\ng: 0x1F\nh: \"a\\tb\"\ni: 2024-01-01\nj: -0\nk: +12\n",
        );
        let PhpValue::Array(items) = doc else {
            panic!()
        };
        let vals: Vec<PhpValue> = items.into_iter().map(|(_, v)| v).collect();
        assert_eq!(
            vals,
            vec![
                PhpValue::Int(1000),
                PhpValue::Str("0644".into()),
                PhpValue::Str("yes".into()),
                PhpValue::Float(1.5),
                PhpValue::Null,
                PhpValue::Str("0x1F".into()),
                PhpValue::Int(31),
                PhpValue::Str("a\tb".into()),
                PhpValue::Int(1_704_067_200),
                PhpValue::Str("-0".into()),
                PhpValue::Int(12),
            ]
        );
    }

    #[test]
    fn structures_and_anchors() {
        let doc = p(
            "x: &base\n  image: nginx\n  ports: [80, '443']\ny:\n  <<: *base\n  image: caddy\nlist:\n  - a\n  - b: c\n    d: e\n\"8080\": z\n",
        );
        assert_eq!(
            crate::yaml::dump(&doc, 10),
            "x:\n    image: nginx\n    ports:\n        - 80\n        - '443'\n'y':\n    image: caddy\n    ports:\n        - 80\n        - '443'\nlist:\n    - a\n    -\n        b: c\n        d: e\n8080: z\n"
        );
    }

    #[test]
    fn errors_have_upstream_messages() {
        let e = parse("a: 1\na: 2\n", 256).unwrap_err();
        assert_eq!(
            e.message(),
            "Duplicate key \"a\" detected at line 2 (near \"a: 2\")."
        );
        let e = parse("services:\n\tweb: x\n", 256).unwrap_err();
        assert!(e.message().contains("tabs"), "{}", e.message());
    }

    #[test]
    fn block_scalars() {
        let doc = p("a: |\n  line1\n  line2\nb: >-\n  folded\n  text\n");
        assert_eq!(
            doc,
            PhpValue::map([
                ("a", PhpValue::from("line1\nline2\n")),
                ("b", PhpValue::from("folded text")),
            ])
        );
    }
}
