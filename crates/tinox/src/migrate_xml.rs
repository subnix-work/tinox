//! XML changelog input (issue #418). A small XML reader plus a mapping into the exact
//! `serde_yaml::Value` shape the YAML changelog parses to, so validation, SQL generation
//! and checksums in `migrate.rs` are shared and identical for both formats.

use serde_yaml::{Mapping, Value};

struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
    line: usize,
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn line(&self) -> usize {
        1 + self.s[..self.i.min(self.s.len())].iter().filter(|&&b| b == b'\n').count()
    }

    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("line {}: {msg}", self.line()))
    }

    fn starts(&self, p: &str) -> bool {
        self.s[self.i..].starts_with(p.as_bytes())
    }

    fn skip_ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn skip_until(&mut self, end: &str, what: &str) -> Result<(), String> {
        while self.i < self.s.len() {
            if self.starts(end) {
                self.i += end.len();
                return Ok(());
            }
            self.i += 1;
        }
        self.err(&format!("unterminated {what}"))
    }

    /// Comments, processing instructions and a DOCTYPE are skipped between nodes.
    fn skip_misc(&mut self) -> Result<(), String> {
        loop {
            self.skip_ws();
            if self.starts("<!--") {
                self.skip_until("-->", "comment")?;
            } else if self.starts("<?") {
                self.skip_until("?>", "processing instruction")?;
            } else if self.starts("<!DOCTYPE") {
                if self.s[self.i..].iter().take_while(|&&b| b != b'>').any(|&b| b == b'[') {
                    return self.err("DOCTYPE with an internal subset is not supported");
                }
                self.skip_until(">", "DOCTYPE")?;
            } else {
                return Ok(());
            }
        }
    }

    fn name(&mut self) -> Result<String, String> {
        let start = self.i;
        while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || b"_-.:".contains(&self.s[self.i])) {
            self.i += 1;
        }
        if start == self.i {
            return self.err("expected a name");
        }
        Ok(String::from_utf8_lossy(&self.s[start..self.i]).into_owned())
    }

    fn entity(&mut self, out: &mut String) -> Result<(), String> {
        let rest = &self.s[self.i..];
        let end = rest.iter().take(12).position(|&b| b == b';');
        let Some(end) = end else { return self.err("unterminated entity reference") };
        let ent = String::from_utf8_lossy(&rest[1..end]).into_owned();
        let ch = match ent.as_str() {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            e if e.starts_with("#x") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32).unwrap_or('\0'),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32).unwrap_or('\0'),
            _ => '\0',
        };
        if ch == '\0' {
            return self.err(&format!("unknown or invalid entity '&{ent};'"));
        }
        out.push(ch);
        self.i += end + 1;
        Ok(())
    }

    fn attr_value(&mut self) -> Result<String, String> {
        let q = *self.s.get(self.i).unwrap_or(&0);
        if q != b'"' && q != b'\'' {
            return self.err("attribute value must be quoted");
        }
        self.i += 1;
        let mut raw = Vec::new();
        let mut out = String::new();
        loop {
            let Some(&b) = self.s.get(self.i) else { return self.err("unterminated attribute value") };
            if b == q {
                self.i += 1;
                break;
            }
            if b == b'<' {
                return self.err("'<' is not allowed in an attribute value");
            }
            if b == b'&' {
                out.push_str(&String::from_utf8_lossy(&raw));
                raw.clear();
                self.entity(&mut out)?;
            } else {
                raw.push(b);
                self.i += 1;
            }
        }
        out.push_str(&String::from_utf8_lossy(&raw));
        Ok(out)
    }

    fn element(&mut self) -> Result<Node, String> {
        let line = self.line();
        self.i += 1; // '<'
        let name = self.name()?;
        let mut attrs: Vec<(String, String)> = Vec::new();
        loop {
            self.skip_ws();
            match self.s.get(self.i) {
                Some(b'/') => {
                    if !self.starts("/>") {
                        return self.err("malformed tag");
                    }
                    self.i += 2;
                    return Ok(Node { name, attrs, children: vec![], text: String::new(), line });
                }
                Some(b'>') => {
                    self.i += 1;
                    break;
                }
                Some(_) => {
                    let k = self.name()?;
                    self.skip_ws();
                    if self.s.get(self.i) != Some(&b'=') {
                        return self.err(&format!("attribute '{k}' has no value"));
                    }
                    self.i += 1;
                    self.skip_ws();
                    let v = self.attr_value()?;
                    if attrs.iter().any(|(n, _)| *n == k) {
                        return self.err(&format!("duplicate attribute '{k}' on <{name}>"));
                    }
                    attrs.push((k, v));
                }
                None => return self.err(&format!("unterminated tag <{name}>")),
            }
        }
        let mut children = Vec::new();
        let mut text = String::new();
        let mut raw = Vec::new();
        loop {
            let Some(&b) = self.s.get(self.i) else { return self.err(&format!("missing </{name}>")) };
            if b == b'<' {
                text.push_str(&String::from_utf8_lossy(&raw));
                raw.clear();
                if self.starts("<!--") {
                    self.skip_until("-->", "comment")?;
                } else if self.starts("<![CDATA[") {
                    self.i += 9;
                    let start = self.i;
                    self.skip_until("]]>", "CDATA section")?;
                    text.push_str(&String::from_utf8_lossy(&self.s[start..self.i - 3]));
                } else if self.starts("<?") {
                    self.skip_until("?>", "processing instruction")?;
                } else if self.starts("</") {
                    self.i += 2;
                    let close = self.name()?;
                    self.skip_ws();
                    if close != name || self.s.get(self.i) != Some(&b'>') {
                        return self.err(&format!("expected </{name}>, found </{close}>"));
                    }
                    self.i += 1;
                    return Ok(Node { name, attrs, children, text, line });
                } else {
                    children.push(self.element()?);
                }
            } else if b == b'&' {
                text.push_str(&String::from_utf8_lossy(&raw));
                raw.clear();
                self.entity(&mut text)?;
            } else {
                raw.push(b);
                self.i += 1;
            }
        }
    }
}

fn parse_xml(text: &str) -> Result<Node, String> {
    let mut r = Reader { s: text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes(), i: 0 };
    r.skip_misc()?;
    if r.s.get(r.i) != Some(&b'<') {
        return r.err("expected the <databaseChangeLog> root element");
    }
    let root = r.element()?;
    r.skip_misc()?;
    if r.i < r.s.len() {
        return r.err("unexpected content after the root element");
    }
    Ok(root)
}

/// Attribute text as the YAML scalar it would have been: booleans and canonical numbers are
/// typed, everything else stays text (so `007` and `1e3` remain strings, `42` equals YAML `42`).
fn scalar(text: &str) -> Value {
    match text {
        "true" => return Value::Bool(true),
        "false" => return Value::Bool(false),
        _ => {}
    }
    if let Ok(n) = text.parse::<i64>() {
        if n.to_string() == text {
            return Value::from(n);
        }
    }
    if text.contains('.') {
        if let Ok(f) = text.parse::<f64>() {
            if f.is_finite() && f.to_string() == text {
                return Value::from(f);
            }
        }
    }
    Value::String(text.to_string())
}

fn key(k: &str) -> Value {
    Value::String(k.to_string())
}

fn single(k: &str, v: Value) -> Value {
    let mut m = Mapping::new();
    m.insert(key(k), v);
    Value::Mapping(m)
}

fn ctx(n: &Node) -> String {
    format!("<{}> (line {})", n.name, n.line)
}

fn require_no_text(n: &Node) -> Result<(), String> {
    if n.text.trim().is_empty() {
        Ok(())
    } else {
        Err(format!("{}: unexpected text content", ctx(n)))
    }
}

fn require_no_children(n: &Node) -> Result<(), String> {
    match n.children.first() {
        None => Ok(()),
        Some(c) => Err(format!("{}: unexpected child element <{}>", ctx(n), c.name)),
    }
}

fn attrs_into(n: &Node, m: &mut Mapping) {
    for (k, v) in &n.attrs {
        m.insert(key(k), scalar(v));
    }
}

/// `<column name=".." type=".."><constraints .../></column>` of createTable / addColumn.
fn column_def(n: &Node) -> Result<Value, String> {
    if n.name != "column" {
        return Err(format!("{}: expected <column>", ctx(n)));
    }
    require_no_text(n)?;
    let mut col = Mapping::new();
    attrs_into(n, &mut col);
    for c in &n.children {
        if c.name != "constraints" {
            return Err(format!("{}: unexpected child element <{}> (expected <constraints>)", ctx(n), c.name));
        }
        require_no_text(c)?;
        require_no_children(c)?;
        if col.contains_key(key("constraints")) {
            return Err(format!("{}: more than one <constraints>", ctx(n)));
        }
        let mut cons = Mapping::new();
        attrs_into(c, &mut cons);
        col.insert(key("constraints"), Value::Mapping(cons));
    }
    Ok(single("column", Value::Mapping(col)))
}

fn attr<'a>(n: &'a Node, k: &str) -> Option<&'a str> {
    n.attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
}

fn change(n: &Node) -> Result<Value, String> {
    let mut spec = Mapping::new();
    match n.name.as_str() {
        "createTable" | "addColumn" => {
            require_no_text(n)?;
            attrs_into(n, &mut spec);
            let cols = n.children.iter().map(column_def).collect::<Result<Vec<_>, _>>()?;
            spec.insert(key("columns"), Value::Sequence(cols));
        }
        "createIndex" => {
            require_no_text(n)?;
            attrs_into(n, &mut spec);
            let mut cols = Vec::new();
            for c in &n.children {
                if c.name != "column" {
                    return Err(format!("{}: unexpected child element <{}> (expected <column>)", ctx(n), c.name));
                }
                require_no_text(c)?;
                require_no_children(c)?;
                let name = attr(c, "name").ok_or_else(|| format!("{}: missing 'name'", ctx(c)))?;
                if c.attrs.len() != 1 {
                    return Err(format!("{}: only 'name' is allowed", ctx(c)));
                }
                cols.push(scalar(name));
            }
            spec.insert(key("columns"), Value::Sequence(cols));
        }
        "insert" => {
            require_no_text(n)?;
            attrs_into(n, &mut spec);
            let mut vals = Mapping::new();
            for c in &n.children {
                if c.name != "column" {
                    return Err(format!("{}: unexpected child element <{}> (expected <column>)", ctx(n), c.name));
                }
                require_no_text(c)?;
                require_no_children(c)?;
                let name = attr(c, "name").ok_or_else(|| format!("{}: missing 'name'", ctx(c)))?;
                let value = attr(c, "value").ok_or_else(|| format!("{}: missing 'value'", ctx(c)))?;
                if c.attrs.len() != 2 {
                    return Err(format!("{}: only 'name' and 'value' are allowed", ctx(c)));
                }
                if vals.insert(key(name), scalar(value)).is_some() {
                    return Err(format!("{}: duplicate column '{name}'", ctx(c)));
                }
            }
            spec.insert(key("values"), Value::Mapping(vals));
        }
        "sql" => {
            let stmts = if n.children.is_empty() {
                Value::String(n.text.clone())
            } else {
                require_no_text(n)?;
                let mut list = Vec::new();
                for c in &n.children {
                    if c.name != "statement" {
                        return Err(format!("{}: unexpected child element <{}> (expected <statement>)", ctx(n), c.name));
                    }
                    require_no_children(c)?;
                    if !c.attrs.is_empty() {
                        return Err(format!("{}: <statement> takes no attributes", ctx(c)));
                    }
                    list.push(Value::String(c.text.clone()));
                }
                Value::Sequence(list)
            };
            if n.attrs.is_empty() {
                return Ok(single("sql", stmts));
            }
            if n.attrs.iter().any(|(k, _)| k != "dbms") {
                return Err(format!("{}: unknown attribute (allowed: dbms)", ctx(n)));
            }
            attrs_into(n, &mut spec);
            spec.insert(key("statements"), stmts);
        }
        // dropTable, renameTable, dropColumn, renameColumn, dropIndex, and anything unknown:
        // attributes only; the shared translator rejects an unsupported type.
        _ => {
            require_no_text(n)?;
            require_no_children(n)?;
            attrs_into(n, &mut spec);
        }
    }
    Ok(single(&n.name, Value::Mapping(spec)))
}

fn change_set(n: &Node) -> Result<Value, String> {
    require_no_text(n)?;
    let mut m = Mapping::new();
    attrs_into(n, &mut m);
    // The id is text in a changelog even when it looks like a number (YAML `id: 1` and XML
    // `id="1"` both yield the number 1; scalar() already guarantees that).
    let mut changes = Vec::new();
    for c in &n.children {
        if c.name == "comment" {
            require_no_children(c)?;
            m.insert(key("comment"), Value::String(c.text.trim().to_string()));
        } else {
            changes.push(change(c)?);
        }
    }
    m.insert(key("changes"), Value::Sequence(changes));
    Ok(Value::Mapping(m))
}

/// Parse an XML changelog into the same value tree `serde_yaml` produces for the YAML form.
pub fn parse_changelog(text: &str) -> Result<Value, String> {
    let root = parse_xml(text)?;
    if root.name != "databaseChangeLog" {
        return Err(format!("{}: the root element must be <databaseChangeLog>", ctx(&root)));
    }
    require_no_text(&root)?;
    // Schema/namespace declarations are accepted (Liquibase editors add them) and ignored.
    if let Some((k, _)) = root.attrs.iter().find(|(k, _)| !(k == "xmlns" || k.starts_with("xmlns:") || k.starts_with("xsi:"))) {
        return Err(format!("{}: unknown attribute '{k}'", ctx(&root)));
    }
    let mut items = Vec::new();
    for c in &root.children {
        match c.name.as_str() {
            "changeSet" => items.push(single("changeSet", change_set(c)?)),
            "include" => {
                require_no_text(c)?;
                require_no_children(c)?;
                let mut m = Mapping::new();
                attrs_into(c, &mut m);
                items.push(single("include", Value::Mapping(m)));
            }
            other => {
                return Err(format!("{}: unknown entry <{other}> (expected changeSet or include)", ctx(c)));
            }
        }
    }
    Ok(single("databaseChangeLog", Value::Sequence(items)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_equals_yaml_value() {
        let xml = r#"<?xml version="1.0"?>
<!-- c -->
<databaseChangeLog xmlns="http://www.liquibase.org/xml/ns/dbchangelog">
  <include file="more.xml"/>
  <changeSet id="1" author="a">
    <createTable tableName="person">
      <column name="id" type="bigint" autoIncrement="true"><constraints primaryKey="true"/></column>
      <column name="note" type="varchar(10)" defaultValue="a&amp;b"/>
    </createTable>
    <createIndex tableName="person" indexName="i" unique="true"><column name="note"/></createIndex>
    <insert tableName="person"><column name="note" value="x"/><column name="id" value="7"/></insert>
    <sql dbms="sqlite"><![CDATA[UPDATE person SET note = 'a<b']]></sql>
  </changeSet>
</databaseChangeLog>"#;
        let yaml = r#"
databaseChangeLog:
  - include: { file: more.xml }
  - changeSet:
      id: 1
      author: a
      changes:
        - createTable:
            tableName: person
            columns:
              - column: { name: id, type: bigint, autoIncrement: true, constraints: { primaryKey: true } }
              - column: { name: note, type: varchar(10), defaultValue: "a&b" }
        - createIndex: { tableName: person, indexName: i, unique: true, columns: [note] }
        - insert: { tableName: person, values: { note: x, id: 7 } }
        - sql: { dbms: sqlite, statements: "UPDATE person SET note = 'a<b'" }
"#;
        let from_xml = parse_changelog(xml).unwrap();
        let from_yaml: Value = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(serde_json::to_string(&from_xml).unwrap(), serde_json::to_string(&from_yaml).unwrap());
    }

    #[test]
    fn malformed_xml_is_a_hard_error() {
        assert!(parse_changelog("<databaseChangeLog><changeSet id='1'></databaseChangeLog>").is_err());
        assert!(parse_changelog("<other/>").is_err());
        assert!(parse_changelog("<databaseChangeLog><bogus/></databaseChangeLog>").is_err());
        assert!(parse_changelog("<databaseChangeLog/><x/>").is_err());
        assert!(parse_changelog("<databaseChangeLog a=1/>").is_err());
        assert!(parse_changelog("<databaseChangeLog>&nope;</databaseChangeLog>").is_err());
    }
}
