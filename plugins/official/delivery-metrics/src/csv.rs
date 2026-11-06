//! CSV as RFC 4180 has it (CRLF line ends, fields with commas, quotes or line breaks quoted, quotes
//! doubled), with protection against formula injection: a field that starts with `=`, `+`, `-`, `@`, a
//! tab or a carriage return gets a leading `'`, so spreadsheets show it as text instead of running it.

/// One field, protected and quoted as needed.
pub fn field(value: &str) -> String {
    let protected = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    if protected.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", protected.replace('"', "\"\""))
    } else {
        protected
    }
}

/// A CSV document under construction.
#[derive(Debug, Default)]
pub struct Writer {
    out: String,
}

impl Writer {
    pub fn new(header: &[&str]) -> Self {
        let mut w = Self::default();
        w.row(header.iter().map(|s| (*s).to_owned()));
        w
    }

    pub fn row<I: IntoIterator<Item = String>>(&mut self, fields: I) {
        let line: Vec<String> = fields.into_iter().map(|f| field(&f)).collect();
        self.out.push_str(&line.join(","));
        self.out.push_str("\r\n");
    }

    pub fn finish(self) -> String {
        self.out
    }
}

/// An optional number as a field: empty when there is none.
pub fn opt<T: ToString>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_as_rfc_4180_says() {
        assert_eq!(field("plain"), "plain");
        assert_eq!(field(""), "");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("two\nlines"), "\"two\nlines\"");
        assert_eq!(field("ünïcode"), "ünïcode");
        assert_eq!(field("12.5"), "12.5");
    }

    #[test]
    fn formulas_are_neutralized() {
        assert_eq!(field("=1+1"), "'=1+1");
        assert_eq!(field("+cmd"), "'+cmd");
        assert_eq!(field("-2"), "'-2");
        assert_eq!(field("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(field("\tx"), "'\tx");
        assert_eq!(field("\rx"), "\"'\rx\"");
        assert_eq!(
            field("=HYPERLINK(\"http://x\",\"y\")"),
            "\"'=HYPERLINK(\"\"http://x\"\",\"\"y\"\")\""
        );
        assert_eq!(field("a=b"), "a=b");
    }

    #[test]
    fn writes_rows_with_crlf() {
        let mut w = Writer::new(&["name", "count"]);
        w.row(["web, api".to_owned(), "3".to_owned()]);
        w.row([String::new(), opt::<i64>(None)]);
        assert_eq!(w.finish(), "name,count\r\n\"web, api\",3\r\n,\r\n");
    }
}
