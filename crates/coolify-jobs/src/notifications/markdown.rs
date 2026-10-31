//! `Illuminate\Mail\Markdown::parse`: league/commonmark's HTML output, with
//! `allow_unsafe_links` off and the table extension on, rendered from
//! pulldown-cmark events.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// `League\CommonMark\Util\Xml::escape`.
fn xml_escape(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// `UrlEncoder::unescapeAndEncode`.
fn url_encode(uri: &str) -> String {
    let safe = !uri.is_empty()
        && uri
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "~!@#$&*()-_=+;:,./?".contains(c));
    if safe {
        return uri.to_owned();
    }
    let chars: Vec<char> = uri.chars().collect();
    let mut out = String::with_capacity(uri.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '%'
            && i + 2 < chars.len()
            && chars[i + 1].is_ascii_hexdigit()
            && chars[i + 2].is_ascii_hexdigit()
        {
            out.push('%');
            out.push(chars[i + 1]);
            out.push(chars[i + 2]);
            i += 3;
            continue;
        }
        if c.is_ascii() {
            if c.is_ascii_alphanumeric() || "!#$&'()*+,-./:;=?@_~".contains(c) {
                out.push(c);
            } else {
                out.push_str(&format!("%{:02X}", c as u32));
            }
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        i += 1;
    }
    out
}

/// `RegexHelper::isLinkPotentiallyUnsafe`.
fn unsafe_link(url: &str) -> bool {
    let cleaned: String = url
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let cleaned = cleaned.trim_start_matches(|c: char| (c as u32) <= 0x20);
    let lower = cleaned.to_ascii_lowercase();
    let unsafe_scheme = ["javascript:", "vbscript:", "file:", "data:"]
        .iter()
        .any(|p| lower.starts_with(p));
    let safe_data = [
        "data:image/png",
        "data:image/gif",
        "data:image/jpeg",
        "data:image/webp",
    ]
    .iter()
    .any(|p| lower.starts_with(p));
    unsafe_scheme && !safe_data
}

fn heading(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "h1",
        HeadingLevel::H2 => "h2",
        HeadingLevel::H3 => "h3",
        HeadingLevel::H4 => "h4",
        HeadingLevel::H5 => "h5",
        HeadingLevel::H6 => "h6",
    }
}

#[derive(Default)]
struct Renderer {
    out: String,
    image_depth: usize,
    pending_title: Option<String>,
    table_align: Vec<Alignment>,
    table_body_open: bool,
    cell: usize,
    in_head: bool,
}

impl Renderer {
    fn nl(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
    }

    fn attr(&mut self, name: &str, value: &str) {
        self.out.push(' ');
        self.out.push_str(name);
        self.out.push_str("=\"");
        xml_escape(&mut self.out, value);
        self.out.push('"');
    }

    fn open_cell(&mut self) {
        let tag = if self.in_head { "th" } else { "td" };
        self.out.push('<');
        self.out.push_str(tag);
        match self.table_align.get(self.cell) {
            Some(Alignment::Left) => self.out.push_str(" align=\"left\""),
            Some(Alignment::Center) => self.out.push_str(" align=\"center\""),
            Some(Alignment::Right) => self.out.push_str(" align=\"right\""),
            _ => {}
        }
        self.out.push('>');
    }

    fn start(&mut self, tag: Tag<'_>) {
        if self.image_depth > 0 {
            if matches!(tag, Tag::Image { .. }) {
                self.image_depth += 1;
            }
            return;
        }
        match tag {
            // pulldown-cmark emits no paragraph events in a tight list.
            Tag::Paragraph => {
                self.nl();
                self.out.push_str("<p>");
            }
            Tag::Heading { level, .. } => {
                self.nl();
                self.out.push('<');
                self.out.push_str(heading(level));
                self.out.push('>');
            }
            Tag::BlockQuote(_) => {
                self.nl();
                self.out.push_str("<blockquote>\n");
            }
            Tag::CodeBlock(kind) => {
                self.nl();
                self.out.push_str("<pre><code");
                if let CodeBlockKind::Fenced(info) = kind {
                    let lang = info.split_whitespace().next().unwrap_or("");
                    if !lang.is_empty() {
                        self.out.push_str(" class=\"language-");
                        xml_escape(&mut self.out, lang);
                        self.out.push('"');
                    }
                }
                self.out.push('>');
            }
            Tag::HtmlBlock => self.nl(),
            Tag::List(start) => {
                self.nl();
                match start {
                    Some(1) => self.out.push_str("<ol>\n"),
                    Some(n) => self.out.push_str(&format!("<ol start=\"{n}\">\n")),
                    None => self.out.push_str("<ul>\n"),
                }
            }
            Tag::Item => {
                self.nl();
                self.out.push_str("<li>");
            }
            Tag::Emphasis => self.out.push_str("<em>"),
            Tag::Strong => self.out.push_str("<strong>"),
            Tag::Link {
                dest_url, title, ..
            } => {
                self.out.push_str("<a");
                let url = url_encode(&dest_url);
                if !unsafe_link(&url) {
                    self.attr("href", &url);
                }
                if !title.is_empty() {
                    self.attr("title", &title);
                }
                self.out.push('>');
            }
            Tag::Image {
                dest_url, title, ..
            } => {
                self.out.push_str("<img");
                let url = url_encode(&dest_url);
                let src = if unsafe_link(&url) { "" } else { url.as_str() };
                self.attr("src", src);
                self.out.push_str(" alt=\"");
                self.image_depth = 1;
                // The title follows the alt text, which is collected from the events.
                self.pending_title = (!title.is_empty()).then(|| title.to_string());
            }
            Tag::Table(align) => {
                self.nl();
                self.table_align = align;
                self.out.push_str("<table>\n");
            }
            Tag::TableHead => {
                self.in_head = true;
                self.cell = 0;
                self.out.push_str("<thead>\n<tr>\n");
            }
            Tag::TableRow => {
                self.cell = 0;
                if !self.table_body_open {
                    self.table_body_open = true;
                    self.out.push_str("<tbody>\n");
                }
                self.out.push_str("<tr>\n");
            }
            Tag::TableCell => self.open_cell(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        if self.image_depth > 0 {
            if matches!(tag, TagEnd::Image) {
                self.image_depth -= 1;
                if self.image_depth == 0 {
                    self.out.push('"');
                    if let Some(t) = self.pending_title.take() {
                        self.attr("title", &t);
                    }
                    self.out.push_str(" />");
                }
            }
            return;
        }
        match tag {
            TagEnd::Paragraph => self.out.push_str("</p>\n"),
            TagEnd::Heading(level) => {
                self.out.push_str("</");
                self.out.push_str(heading(level));
                self.out.push_str(">\n");
            }
            TagEnd::BlockQuote(_) => {
                self.nl();
                self.out.push_str("</blockquote>\n");
            }
            TagEnd::CodeBlock => {
                // The code of a block always ends with a new line, also on the last line of
                // the document.
                if !self.out.ends_with('\n') && !self.out.ends_with('>') {
                    self.out.push('\n');
                }
                self.out.push_str("</code></pre>\n");
            }
            TagEnd::HtmlBlock => self.nl(),
            TagEnd::List(ordered) => {
                self.nl();
                self.out
                    .push_str(if ordered { "</ol>\n" } else { "</ul>\n" });
            }
            TagEnd::Item => {
                self.out.push_str("</li>\n");
            }
            TagEnd::Emphasis => self.out.push_str("</em>"),
            TagEnd::Strong => self.out.push_str("</strong>"),
            TagEnd::Link => self.out.push_str("</a>"),
            TagEnd::Table => {
                if self.table_body_open {
                    self.out.push_str("</tbody>\n");
                    self.table_body_open = false;
                }
                self.out.push_str("</table>\n");
            }
            TagEnd::TableHead => {
                self.in_head = false;
                self.out.push_str("</tr>\n</thead>\n");
            }
            TagEnd::TableRow => self.out.push_str("</tr>\n"),
            TagEnd::TableCell => {
                self.out
                    .push_str(if self.in_head { "</th>\n" } else { "</td>\n" });
                self.cell += 1;
            }
            _ => {}
        }
    }
}

/// `Markdown::parse($text)->toHtml()`.
pub fn parse(text: &str) -> String {
    let mut r = Renderer::default();
    for ev in Parser::new_ext(text, Options::ENABLE_TABLES) {
        match ev {
            Event::Start(tag) => r.start(tag),
            Event::End(tag) => r.end(tag),
            Event::Text(t) => {
                xml_escape(&mut r.out, &t);
            }
            Event::Code(t) => {
                if r.image_depth > 0 {
                    xml_escape(&mut r.out, &t);
                } else {
                    r.out.push_str("<code>");
                    xml_escape(&mut r.out, &t);
                    r.out.push_str("</code>");
                }
            }
            Event::Html(t) | Event::InlineHtml(t) if r.image_depth == 0 => r.out.push_str(&t),
            Event::SoftBreak => r.out.push_str(if r.image_depth > 0 { " " } else { "\n" }),
            Event::HardBreak => r
                .out
                .push_str(if r.image_depth > 0 { " " } else { "<br />\n" }),
            Event::Rule => {
                r.nl();
                r.out.push_str("<hr />\n");
            }
            _ => {}
        }
    }
    r.out
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn basics() {
        assert_eq!(parse("a\nb\n\n---\n"), "<p>a\nb</p>\n<hr />\n");
        assert_eq!(parse("### Reason\n\nx"), "<h3>Reason</h3>\n<p>x</p>\n");
        assert_eq!(
            parse("[a](http://x/?a=1&amp;b=2)"),
            "<p><a href=\"http://x/?a=1&amp;b=2\">a</a></p>\n"
        );
        assert_eq!(parse("[](javascript:alert(1))"), "<p><a></a></p>\n");
        assert_eq!(parse("&#039;&quot;"), "<p>'&quot;</p>\n");
        assert_eq!(parse("- a\n- b\n"), "<ul>\n<li>a</li>\n<li>b</li>\n</ul>\n");
        assert_eq!(
            parse("<pre>\nx &amp; y\n</pre>\n\nz"),
            "<pre>\nx &amp; y\n</pre>\n<p>z</p>\n"
        );
    }
}
