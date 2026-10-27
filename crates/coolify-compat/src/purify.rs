//! The part of HTMLPurifier that upstream's `validation_logs` mutator uses
//! (`Purify::config('validation_logs')`, config/purify.php):
//!
//! - allowed: `a[href|title|target|class]`, `br`, `div[class]`, `pre[class]`,
//!   `span[class]`, `p[class]`; other tags are dropped and their text kept,
//!   except `script`/`style`, which are dropped with their content;
//! - `target` only `_blank`, which adds `rel="noreferrer noopener"`;
//! - `href` only with an allowed scheme (or none);
//! - text is decoded and re-escaped (`&`, `<`, `>` only); `\r\n` becomes `\n`;
//! - unknown tags such as the redactor's `<REDACTED>` disappear.
//!
//! Behaviour was checked against the reference container's purifier.

const ALLOWED: &[(&str, &[&str])] = &[
    ("a", &["href", "title", "target", "class"]),
    ("br", &[]),
    ("div", &["class"]),
    ("pre", &["class"]),
    ("span", &["class"]),
    ("p", &["class"]),
];
/// HTMLPurifier `URI.AllowedSchemes` defaults.
const SCHEMES: &[&str] = &["http", "https", "mailto", "ftp", "nntp", "news", "tel"];

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(';').map(|e| e + 1).filter(|e| *e <= 12);
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ if name.starts_with("#x") || name.starts_with("#X") => {
                    u32::from_str_radix(&name[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                }
                _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            c.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

struct Tag {
    name: String,
    closing: bool,
    attrs: Vec<(String, String)>,
}

/// Parses `<...>` at the start of `s`; returns the tag and its length.
fn parse_tag(s: &str) -> Option<(Tag, usize)> {
    let end = s.find('>')?;
    let inner = &s[1..end];
    let (closing, inner) = match inner.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, inner),
    };
    let inner = inner.trim_end_matches('/');
    let name_end = inner
        .find(|c: char| c.is_ascii_whitespace() || c == '/')
        .unwrap_or(inner.len());
    let name = inner[..name_end].to_ascii_lowercase();
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let mut attrs = Vec::new();
    let mut rest = inner[name_end..].trim_start();
    while !rest.is_empty() {
        let key_end = rest
            .find(|c: char| c == '=' || c.is_ascii_whitespace())
            .unwrap_or(rest.len());
        let key = rest[..key_end].to_ascii_lowercase();
        rest = rest[key_end..].trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (v, len) = match after.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let close = after[1..].find(q).map_or(after.len(), |i| i + 1);
                    (&after[1..close], (close + 1).min(after.len()))
                }
                _ => {
                    let e = after
                        .find(|c: char| c.is_ascii_whitespace())
                        .unwrap_or(after.len());
                    (&after[..e], e)
                }
            };
            rest = after[len..].trim_start();
            decode_entities(v)
        } else {
            key.clone()
        };
        if !key.is_empty() {
            attrs.push((key, value));
        }
    }
    Some((
        Tag {
            name,
            closing,
            attrs,
        },
        end + 1,
    ))
}

fn allowed_href(href: &str) -> bool {
    let h = href.trim();
    match h.find(':') {
        Some(i) if !h[..i].contains(['/', '?', '#']) => {
            SCHEMES.contains(&h[..i].to_ascii_lowercase().as_str())
        }
        _ => true,
    }
}

fn open_tag(tag: &Tag, allowed_attrs: &[&str]) -> String {
    let mut out = format!("<{}", tag.name);
    let mut blank = false;
    for (k, v) in &tag.attrs {
        if !allowed_attrs.contains(&k.as_str()) {
            continue;
        }
        match k.as_str() {
            "href" if !allowed_href(v) => continue,
            "target" if v != "_blank" => continue,
            "target" => blank = true,
            _ => {}
        }
        out.push_str(&format!(" {k}=\"{}\"", escape_attr(v)));
    }
    if blank {
        out.push_str(" rel=\"noreferrer noopener\"");
    }
    out.push('>');
    out
}

/// `Purify::config('validation_logs')->clean($html)`.
pub fn validation_logs(html: &str) -> String {
    let html = html.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(html.len());
    let mut open: Vec<String> = Vec::new();
    let mut skip_until: Option<String> = None;
    let mut text = String::new();
    let flush = |text: &mut String, out: &mut String| {
        out.push_str(&escape_text(&decode_entities(text)));
        text.clear();
    };
    let mut rest = html.as_str();
    while !rest.is_empty() {
        if rest.starts_with("<!--") {
            let end = rest.find("-->").map_or(rest.len(), |i| i + 3);
            rest = &rest[end..];
            continue;
        }
        if rest.starts_with('<')
            && let Some((tag, len)) = parse_tag(rest)
        {
            rest = &rest[len..];
            if let Some(until) = &skip_until {
                if tag.closing && &tag.name == until {
                    skip_until = None;
                }
                continue;
            }
            flush(&mut text, &mut out);
            if !tag.closing && matches!(tag.name.as_str(), "script" | "style") {
                skip_until = Some(tag.name);
                continue;
            }
            let Some((_, attrs)) = ALLOWED.iter().find(|(n, _)| *n == tag.name) else {
                continue;
            };
            if tag.name == "br" {
                if !tag.closing {
                    out.push_str("<br>");
                }
            } else if !tag.closing {
                out.push_str(&open_tag(&tag, attrs));
                open.push(tag.name);
            } else if let Some(pos) = open.iter().rposition(|n| *n == tag.name) {
                for name in open.drain(pos..).rev() {
                    out.push_str(&format!("</{name}>"));
                }
            }
            continue;
        }
        let next = rest[1..].find('<').map_or(rest.len(), |i| i + 1);
        if skip_until.is_none() {
            text.push_str(&rest[..next]);
        }
        rest = &rest[next..];
    }
    flush(&mut text, &mut out);
    for name in open.into_iter().rev() {
        out.push_str(&format!("</{name}>"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::validation_logs as clean;

    // Inputs and outputs from the reference container's purifier.
    #[test]
    fn matches_the_reference() {
        let cases: &[(&str, &str)] = &[
            (
                r#"Server is not reachable. Please validate your configuration and connection.<br>Check this <a target="_blank" class="text-black underline dark:text-white" href="https://coolify.io/docs/knowledge-base/server/openssh">documentation</a> for further help. <br><br><div class="text-error">Error: Load key &quot;ssh_key@abc&quot;: invalid format &#039;x&#039; &lt;y&gt; &amp; z</div>"#,
                r#"Server is not reachable. Please validate your configuration and connection.<br>Check this <a target="_blank" class="text-black underline dark:text-white" href="https://coolify.io/docs/knowledge-base/server/openssh" rel="noreferrer noopener">documentation</a> for further help. <br><br><div class="text-error">Error: Load key "ssh_key@abc": invalid format 'x' &lt;y&gt; &amp; z</div>"#,
            ),
            (
                "Error: <REDACTED>: Permission denied (publickey).",
                "Error: : Permission denied (publickey).",
            ),
            (
                r#"a<script>alert(1)</script>b<b>bold</b><a href="javascript:alert(1)">x</a><a href="https://x.y" onclick="z" target="_self">y</a>"#,
                r#"abbold<a>x</a><a href="https://x.y">y</a>"#,
            ),
            ("multi\nline\r\n\ttab é ünï", "multi\nline\n\ttab é ünï"),
            (
                "Error: password=<REDACTED> token: <REDACTED> Bearer <REDACTED>",
                "Error: password= token:  Bearer ",
            ),
            (
                r#"<div class="text-error">Error: </div>"#,
                r#"<div class="text-error">Error: </div>"#,
            ),
            (
                r#"it's "quoted" & <br/> <BR> done"#,
                r#"it's "quoted" &amp; <br> <br> done"#,
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(clean(input), *expected, "{input}");
        }
    }

    #[test]
    fn closes_and_drops_unbalanced_tags() {
        assert_eq!(
            clean("<div class=\"x\">open"),
            "<div class=\"x\">open</div>"
        );
        assert_eq!(clean("stray</div> end"), "stray end");
        assert_eq!(clean("1 < 2 > 0"), "1 &lt; 2 &gt; 0");
    }
}
