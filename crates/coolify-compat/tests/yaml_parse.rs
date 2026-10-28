//! `yaml_parse::parse` against `Symfony\Component\Yaml\Yaml::parse()` 8.1 on the reference instance
//! (`tests/fixtures/yaml_parse/probe.php` printed `expected.txt`): the parsed value as PHP
//! `serialize()` (types and key types included), its `Yaml::dump($value, 10, 2)`, or the
//! `ParseException` message.

#![allow(clippy::unwrap_used)]

use coolify_compat::yaml::{Key, PhpValue, dump_with_indent};

/// PHP `serialize()`.
fn serialize(v: &PhpValue) -> String {
    match v {
        PhpValue::Null => "N;".into(),
        PhpValue::Bool(b) => format!("b:{};", u8::from(*b)),
        PhpValue::Int(i) => format!("i:{i};"),
        PhpValue::Float(f) => {
            if f.is_nan() {
                "d:NAN;".into()
            } else if f.is_infinite() {
                if *f > 0.0 {
                    "d:INF;".into()
                } else {
                    "d:-INF;".into()
                }
            } else if f.fract() == 0.0 && f.abs() < 1e15 {
                format!("d:{};", *f as i64)
            } else {
                format!("d:{f};")
            }
        }
        PhpValue::Str(s) => format!("s:{}:\"{s}\";", s.len()),
        PhpValue::Array(items) => {
            let mut out = format!("a:{}:{{", items.len());
            for (k, v) in items {
                match k {
                    Key::Int(i) => out.push_str(&format!("i:{i};")),
                    Key::Str(s) => out.push_str(&format!("s:{}:\"{s}\";", s.len())),
                }
                out.push_str(&serialize(v));
            }
            out.push('}');
            out
        }
    }
}

#[test]
fn parses_like_symfony() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yaml_parse");
    let mut files: Vec<_> = std::fs::read_dir(dir.join("cases"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    let mut out = String::new();
    for f in files {
        let input = std::fs::read_to_string(&f).unwrap();
        let body = match coolify_compat::yaml_parse::parse(&input, 256) {
            Ok(v) => format!(
                "OK {}\n---DUMP---\n{}",
                serialize(&v),
                if matches!(v, PhpValue::Array(_)) {
                    dump_with_indent(&v, 10, 2)
                } else {
                    String::new()
                }
            ),
            Err(e) => format!(
                "ERR Symfony\\Component\\Yaml\\Exception\\ParseException: {}",
                e.message()
            ),
        };
        out.push_str(&format!(
            "=== {}\n{body}\n",
            f.file_name().unwrap().to_string_lossy()
        ));
    }
    let expected = std::fs::read_to_string(dir.join("expected.txt")).unwrap();
    assert_eq!(out, expected);
}
