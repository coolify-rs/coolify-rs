//! The developer view of environment variables: `formatEnvValueForDevView`,
//! `formatEnvVariableForDevView` and `parseEnvDevView` (`bootstrap/helpers/shared.php`).

/// `EnvironmentVariable::DEV_VIEW_FLAG_DEFAULTS`: the flags a `# coolify:` line can set.
pub const ENV_FLAGS: &[(&str, bool)] =
    &[("literal", false), ("buildtime", true), ("runtime", true)];

/// `SharedEnvironmentVariable::DEV_VIEW_FLAG_DEFAULTS`: shared variables have no build or run flags.
pub const SHARED_FLAGS: &[(&str, bool)] = &[("literal", false)];

const DIRECTIVE: &str = "# coolify:";

/// `formatEnvValueForDevView`: the value of a `KEY=VALUE` line that `parseEnvFormatToArray` reads
/// back unchanged, or None when the format cannot hold it (a line break, or quotes needed and
/// both quote types inside).
pub fn format_value(value: Option<&str>) -> Option<String> {
    let value = value.unwrap_or_default();
    if value.contains('\n') {
        return None;
    }
    // `preg_match('/\s#/')`: PCRE's `\s` without the `u` flag.
    let bytes = value.as_bytes();
    let space_hash = bytes
        .windows(2)
        .any(|w| matches!(w[0], b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r') && w[1] == b'#');
    let needs_quotes = value.starts_with('"') || value.starts_with('\'') || space_hash;
    if !needs_quotes {
        return Some(value.to_owned());
    }
    if !value.contains('\'') {
        return Some(format!("'{value}'"));
    }
    if !value.contains('"') {
        return Some(format!("\"{value}\""));
    }
    None
}

/// One variable as `formatEnvVariableForDevView` reads it.
#[derive(Debug, Clone, Copy)]
pub struct DevVariable<'a> {
    pub key: &'a str,
    pub value: Option<&'a str>,
    pub is_shown_once: bool,
    pub is_multiline: bool,
    /// The current value of each flag, by flag name (`literal`, ...); a missing one is false.
    pub flags: &'a [(&'a str, bool)],
}

/// `formatEnvVariableForDevView`: the `KEY=value` line, with a `# coolify:` line above it when a
/// flag is not its default. A variable the view cannot edit gets a placeholder and no directive.
pub fn format_variable(
    v: &DevVariable<'_>,
    can_view_value: bool,
    defaults: &[(&str, bool)],
) -> String {
    let key = v.key;
    if !can_view_value {
        return format!("{key}=(Hidden, only admins can view)");
    }
    if v.is_shown_once {
        return format!("{key}=(Locked Secret, delete and add again to change)");
    }
    if v.is_multiline {
        return format!("{key}=(Multiline environment variable, edit in normal view)");
    }
    let Some(value) = format_value(v.value) else {
        return format!("{key}=(Value cannot be shown in this view, edit in normal view)");
    };
    let flags: Vec<String> = defaults
        .iter()
        .filter(|(name, default)| v.flags.iter().any(|(n, on)| n == name && *on) != *default)
        .map(|(name, default)| {
            if *default {
                format!("{name}=false")
            } else {
                (*name).to_owned()
            }
        })
        .collect();
    let line = format!("{key}={value}");
    if flags.is_empty() {
        line
    } else {
        format!("{DIRECTIVE} {}\n{line}", flags.join(" "))
    }
}

/// One variable of `parseEnvDevView`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub key: String,
    pub value: String,
    pub comment: Option<String>,
    /// The flags of the `# coolify:` line directly above the key line, as model attributes
    /// (`is_literal`, ...), in the order the line names them. Empty: the flags stay as they are.
    pub settings: Vec<(String, bool)>,
}

impl Parsed {
    pub fn setting(&self, attribute: &str) -> Option<bool> {
        self.settings
            .iter()
            .rev()
            .find(|(a, _)| a == attribute)
            .map(|(_, v)| *v)
    }
}

/// `parseEnvDevView`: `parseEnvFormatToArray` with the `# coolify:` flags of each key.
pub fn parse(contents: &str, defaults: &[(&str, bool)]) -> Result<Vec<Parsed>, String> {
    let directly_above = |line: usize| {
        format!("Line {line}: a \"{DIRECTIVE}\" line must be directly above a KEY=value line.")
    };
    let mut settings_by_key: Vec<(String, Vec<(String, bool)>)> = Vec::new();
    let mut directive: Option<Vec<(String, bool)>> = None;
    let mut directive_line = 0;
    for (index, line) in contents.split('\n').enumerate() {
        let is_key_line = !line.is_empty() && !line.starts_with('#') && line.contains('=');
        if directive.is_some() && !is_key_line {
            return Err(directly_above(directive_line));
        }
        if is_key_line {
            let key = &line[..line.find('=').unwrap_or_default()];
            let settings = directive.take().unwrap_or_default();
            match settings_by_key.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = settings,
                None => settings_by_key.push((key.to_owned(), settings)),
            }
        } else if let Some(rest) = line.strip_prefix(DIRECTIVE) {
            directive_line = index + 1;
            // `preg_split('/\s+/', trim(...), -1, PREG_SPLIT_NO_EMPTY)`.
            let flags: Vec<&str> = rest
                .split([' ', '\t', '\n', '\u{b}', '\u{c}', '\r', '\0'])
                .filter(|f| !f.is_empty())
                .collect();
            if flags.is_empty() {
                return Err(format!(
                    "Line {directive_line}: the \"{DIRECTIVE}\" line has no flags."
                ));
            }
            let mut set = Vec::new();
            for flag in flags {
                let (name, value) = flag.split_once('=').unwrap_or((flag, "true"));
                if !defaults.iter().any(|(n, _)| *n == name) {
                    let allowed: Vec<&str> = defaults.iter().map(|(n, _)| *n).collect();
                    return Err(format!(
                        "Line {directive_line}: unknown flag \"{name}\". Allowed flags: {}.",
                        allowed.join(", ")
                    ));
                }
                if value != "true" && value != "false" {
                    return Err(format!(
                        "Line {directive_line}: the value of \"{name}\" must be true or false."
                    ));
                }
                let attribute = format!("is_{name}");
                // A repeated flag keeps its first place with the last value, as an array key.
                match set
                    .iter_mut()
                    .find(|(a, _): &&mut (String, bool)| *a == attribute)
                {
                    Some(slot) => slot.1 = value == "true",
                    None => set.push((attribute, value == "true")),
                }
            }
            directive = Some(set);
        }
    }
    if directive.is_some() {
        return Err(directly_above(directive_line));
    }
    Ok(super::envs::parse_env_format(contents)
        .into_iter()
        .map(|(key, value, comment)| {
            let settings = settings_by_key
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, s)| s.clone())
                .unwrap_or_default();
            Parsed {
                key,
                value,
                comment,
                settings,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_are_quoted_only_when_needed() {
        assert_eq!(format_value(None).as_deref(), Some(""));
        assert_eq!(format_value(Some("plain")).as_deref(), Some("plain"));
        assert_eq!(format_value(Some("a#b")).as_deref(), Some("a#b"));
        assert_eq!(format_value(Some("a #b")).as_deref(), Some("'a #b'"));
        assert_eq!(format_value(Some("\"x\"")).as_deref(), Some("'\"x\"'"));
        assert_eq!(format_value(Some("'x'")).as_deref(), Some("\"'x'\""));
        assert_eq!(format_value(Some("'x\" #")), None);
        assert_eq!(format_value(Some("a\nb")), None);
        // What is written reads back unchanged.
        for v in ["a #b", "\"x\"", "'x'", "x # y"] {
            let line = format!("K={}", format_value(Some(v)).unwrap());
            assert_eq!(parse(&line, ENV_FLAGS).unwrap()[0].value, v);
        }
    }

    #[test]
    fn flags_that_differ_from_their_default_get_a_directive() {
        let v = DevVariable {
            key: "A",
            value: Some("1"),
            is_shown_once: false,
            is_multiline: false,
            flags: &[("literal", true), ("buildtime", false), ("runtime", false)],
        };
        assert_eq!(
            format_variable(&v, true, ENV_FLAGS),
            "# coolify: literal buildtime=false runtime=false\nA=1"
        );
        let v = DevVariable {
            flags: &[("literal", false), ("buildtime", true), ("runtime", true)],
            ..v
        };
        assert_eq!(format_variable(&v, true, ENV_FLAGS), "A=1");
        assert_eq!(
            format_variable(&v, false, ENV_FLAGS),
            "A=(Hidden, only admins can view)"
        );
        let v = DevVariable {
            value: Some("'\" #"),
            ..v
        };
        assert_eq!(
            format_variable(&v, true, ENV_FLAGS),
            "A=(Value cannot be shown in this view, edit in normal view)"
        );
    }

    #[test]
    fn directives_set_the_flags_of_the_next_key() {
        let parsed = parse(
            "# coolify: literal buildtime=false\nA=1\nB=2 # note\n# coolify: runtime=true\nC='x #y'",
            ENV_FLAGS,
        )
        .unwrap();
        assert_eq!(
            parsed,
            vec![
                Parsed {
                    key: "A".into(),
                    value: "1".into(),
                    comment: None,
                    settings: vec![("is_literal".into(), true), ("is_buildtime".into(), false)],
                },
                Parsed {
                    key: "B".into(),
                    value: "2".into(),
                    comment: Some("note".into()),
                    settings: vec![],
                },
                Parsed {
                    key: "C".into(),
                    value: "x #y".into(),
                    comment: None,
                    settings: vec![("is_runtime".into(), true)],
                },
            ]
        );
    }

    #[test]
    fn bad_directives_are_refused() {
        assert_eq!(
            parse("A=1\n# coolify: literal\n\nB=2", ENV_FLAGS).unwrap_err(),
            "Line 2: a \"# coolify:\" line must be directly above a KEY=value line."
        );
        assert_eq!(
            parse("# coolify: literal", ENV_FLAGS).unwrap_err(),
            "Line 1: a \"# coolify:\" line must be directly above a KEY=value line."
        );
        assert_eq!(
            parse("# coolify:   \nA=1", ENV_FLAGS).unwrap_err(),
            "Line 1: the \"# coolify:\" line has no flags."
        );
        assert_eq!(
            parse("# coolify: buildtime\nA=1", SHARED_FLAGS).unwrap_err(),
            "Line 1: unknown flag \"buildtime\". Allowed flags: literal."
        );
        assert_eq!(
            parse("# coolify: literal=yes\nA=1", ENV_FLAGS).unwrap_err(),
            "Line 1: the value of \"literal\" must be true or false."
        );
        // An ordinary comment is not a directive.
        assert!(parse("# coolify literal\nA=1", ENV_FLAGS).is_ok());
    }
}
