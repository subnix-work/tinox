//! Declarative schema migrations (issue #410): a Liquibase-style YAML changelog is
//! parsed into change sets, each change is translated into SQL for the configured
//! driver, and the result is serialized into the payload the runtime's
//! `tinox_migrate_run` applies (see runtime.c). Everything that can be decided
//! without a database (parsing, validation, SQL generation, checksums) lives here,
//! so `tinox db validate` / `update-sql` need no connection.

use serde_yaml::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    Sqlite,
    Postgres,
    Mysql,
}

impl Dialect {
    pub fn from_driver(driver: &str) -> Result<Dialect, String> {
        match driver {
            "sqlite" => Ok(Dialect::Sqlite),
            "postgres" => Ok(Dialect::Postgres),
            "mysql" => Ok(Dialect::Mysql),
            other => Err(format!("unknown [database] driver '{other}' (expected sqlite, postgres or mysql)")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dialect::Sqlite => "sqlite",
            Dialect::Postgres => "postgres",
            Dialect::Mysql => "mysql",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChangeSet {
    pub id: String,
    pub author: String,
    /// Changelog file the change set was declared in (relative to the root changelog's directory).
    pub file: String,
    pub checksum: String,
    pub run_on_change: bool,
    pub run_always: bool,
    pub stmts: Vec<String>,
    /// SQL that undoes the changeSet, or the reason it cannot be undone (checked when a
    /// rollback is requested, never silently skipped).
    pub rollback: Result<Vec<String>, String>,
    /// `tag:` -- `tinox db rollback --tag T` undoes everything applied after this changeSet.
    pub tag: Option<String>,
}

fn scalar_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn get<'a>(m: &'a Value, key: &str) -> Option<&'a Value> {
    m.as_mapping()?.get(Value::String(key.to_string()))
}

fn get_str(m: &Value, key: &str, ctx: &str) -> Result<String, String> {
    match get(m, key) {
        Some(v) => scalar_string(v).ok_or_else(|| format!("{ctx}: '{key}' must be a scalar value")),
        None => Err(format!("{ctx}: missing required '{key}'")),
    }
}

fn get_bool(m: &Value, key: &str, ctx: &str) -> Result<bool, String> {
    match get(m, key) {
        None => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("{ctx}: '{key}' must be true or false")),
    }
}

fn check_keys(m: &Value, allowed: &[&str], ctx: &str) -> Result<(), String> {
    let map = m.as_mapping().ok_or_else(|| format!("{ctx}: expected a mapping"))?;
    for k in map.keys() {
        let name = k.as_str().unwrap_or("");
        if !allowed.contains(&name) {
            return Err(format!("{ctx}: unknown key '{name}' (allowed: {})", allowed.join(", ")));
        }
    }
    Ok(())
}

/// A SQL identifier. Quoted (never concatenated raw) so a name cannot smuggle in SQL.
fn quote_ident(d: Dialect, name: &str, ctx: &str) -> Result<String, String> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("{ctx}: invalid identifier '{name}' (letters, digits and underscore only)"));
    }
    Ok(match d {
        Dialect::Mysql => format!("`{name}`"),
        _ => format!("\"{name}\""),
    })
}

fn sql_literal(v: &Value, ctx: &str) -> Result<String, String> {
    match v {
        Value::Null => Ok("NULL".to_string()),
        Value::Bool(b) => Ok(if *b { "1".into() } else { "0".into() }),
        Value::Number(n) => Ok(n.to_string()),
        Value::String(s) => Ok(format!("'{}'", s.replace('\'', "''"))),
        _ => Err(format!("{ctx}: a value must be a string, number, boolean or null")),
    }
}

thread_local! {
    /// Tinox enums (name -> variant names) visible to `enum(Name)` column types while a changelog loads.
    static ENUMS: std::cell::RefCell<std::collections::BTreeMap<String, Vec<String>>> = Default::default();
}

/// `enum(Name)` -> the variants of the project's Tinox enum `Name`, or a hard error.
fn enum_variants(ty: &str, ctx: &str) -> Result<Option<(String, Vec<String>)>, String> {
    let t = ty.trim();
    let Some(inner) = t.strip_prefix("enum(").and_then(|r| r.strip_suffix(')')) else { return Ok(None) };
    let name = inner.trim();
    ENUMS.with(|e| match e.borrow().get(name) {
        Some(v) if !v.is_empty() => Ok(Some((name.to_string(), v.clone()))),
        Some(_) => Err(format!("{ctx}: Tinox enum '{name}' has no variants")),
        None => Err(format!("{ctx}: unknown Tinox enum '{name}' in type '{ty}' (enum must be declared in the project's .tnx sources)")),
    })
}

fn enum_check_expr(d: Dialect, col: &str, variants: &[String]) -> String {
    let _ = d;
    let list: Vec<String> = variants.iter().map(|v| format!("'{}'", v.replace('\'', "''"))).collect();
    format!("{col} IN ({})", list.join(", "))
}

fn map_type(d: Dialect, ty: &str, ctx: &str) -> Result<String, String> {
    let lower = ty.trim().to_ascii_lowercase();
    let (base, arg) = match lower.find('(') {
        Some(i) if lower.ends_with(')') => (lower[..i].trim().to_string(), Some(lower[i..].to_string())),
        Some(_) => return Err(format!("{ctx}: malformed type '{ty}'")),
        None => (lower.clone(), None),
    };
    let sized = |name: &str| format!("{name}{}", arg.clone().unwrap_or_default());
    let mapped = match (base.as_str(), d) {
        ("bigint", Dialect::Sqlite) | ("int", Dialect::Sqlite) | ("integer", Dialect::Sqlite) => "INTEGER".to_string(),
        ("bigint", Dialect::Postgres) => "BIGINT".into(),
        ("bigint", Dialect::Mysql) => "BIGINT".into(),
        ("int", Dialect::Postgres) | ("integer", Dialect::Postgres) => "INTEGER".into(),
        ("int", Dialect::Mysql) | ("integer", Dialect::Mysql) => "INT".into(),
        ("varchar", _) | ("char", _) => {
            if arg.is_none() {
                return Err(format!("{ctx}: type '{base}' needs a length, e.g. {base}(100)"));
            }
            sized(&base.to_ascii_uppercase())
        }
        ("text", _) => "TEXT".into(),
        ("boolean", Dialect::Postgres) => "BOOLEAN".into(),
        ("boolean", Dialect::Sqlite) => "INTEGER".into(),
        ("boolean", Dialect::Mysql) => "TINYINT(1)".into(),
        ("float", Dialect::Sqlite) | ("double", Dialect::Sqlite) => "REAL".into(),
        ("float", Dialect::Postgres) => "REAL".into(),
        ("double", Dialect::Postgres) => "DOUBLE PRECISION".into(),
        ("float", Dialect::Mysql) => "FLOAT".into(),
        ("double", Dialect::Mysql) => "DOUBLE".into(),
        ("timestamp", _) => "TIMESTAMP".into(),
        ("date", _) => "DATE".into(),
        ("blob", Dialect::Postgres) => "BYTEA".into(),
        ("blob", _) => "BLOB".into(),
        _ => {
            return Err(format!(
                "{ctx}: unsupported type '{ty}' (supported: bigint, int, varchar(n), char(n), text, boolean, float, double, timestamp, date, blob)"
            ))
        }
    };
    Ok(mapped)
}

struct Column {
    name: String,
    ty: String,
    auto_increment: bool,
    primary_key: bool,
    not_null: bool,
    unique: bool,
    default: Option<String>,
    references: Option<(String, String)>,
    /// (constraint name, CHECK expression) for `enum(Name)` columns.
    check: Option<(String, String)>,
}

fn parse_column(d: Dialect, table: &str, v: &Value, ctx: &str) -> Result<Column, String> {
    let col = get(v, "column").ok_or_else(|| format!("{ctx}: each entry must be a 'column:' mapping"))?;
    check_keys(v, &["column"], ctx)?;
    check_keys(col, &["name", "type", "autoIncrement", "defaultValue", "constraints", "enumCheck"], ctx)?;
    let name = get_str(col, "name", ctx)?;
    let cctx = format!("{ctx}, column '{name}'");
    let ty = get_str(col, "type", &cctx)?;
    let enum_ty = enum_variants(&ty, &cctx)?;
    let enum_check = match get(col, "enumCheck") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(format!("{cctx}: 'enumCheck' must be true or false")),
    };
    if enum_ty.is_none() && get(col, "enumCheck").is_some() {
        return Err(format!("{cctx}: 'enumCheck' only applies to enum(Name) columns"));
    }
    let mut c = Column {
        name: quote_ident(d, &name, &cctx)?,
        ty: if enum_ty.is_some() { "VARCHAR(255)".to_string() } else { map_type(d, &ty, &cctx)? },
        auto_increment: get_bool(col, "autoIncrement", &cctx)?,
        primary_key: false,
        not_null: false,
        unique: false,
        default: None,
        references: None,
        check: None,
    };
    if let Some(dv) = get(col, "defaultValue") {
        if let Some((en, vars)) = &enum_ty {
            match dv {
                Value::String(x) if vars.contains(x) => {}
                _ => return Err(format!("{cctx}: defaultValue must be a variant of enum {en} ({})", vars.join(", "))),
            }
        }
        c.default = Some(sql_literal(dv, &cctx)?);
    }
    if let (Some((_, vars)), true) = (&enum_ty, enum_check) {
        c.check = Some((enum_check_name(d, table, &name, &cctx)?, enum_check_expr(d, &c.name, vars)));
    }
    if let Some(cons) = get(col, "constraints") {
        check_keys(cons, &["primaryKey", "nullable", "unique", "references"], &cctx)?;
        c.primary_key = get_bool(cons, "primaryKey", &cctx)?;
        c.unique = get_bool(cons, "unique", &cctx)?;
        match get(cons, "nullable") {
            None => {}
            Some(Value::Bool(b)) => c.not_null = !*b,
            Some(_) => return Err(format!("{cctx}: 'nullable' must be true or false")),
        }
        if let Some(r) = get(cons, "references") {
            let s = scalar_string(r).ok_or_else(|| format!("{cctx}: 'references' must be 'table(column)'"))?;
            let open = s.find('(').filter(|_| s.ends_with(')'));
            let open = open.ok_or_else(|| format!("{cctx}: 'references' must look like table(column), found '{s}'"))?;
            let (t, col) = (s[..open].trim(), s[open + 1..s.len() - 1].trim());
            c.references = Some((quote_ident(d, t, &cctx)?, quote_ident(d, col, &cctx)?));
        }
    }
    if c.auto_increment && !c.primary_key {
        return Err(format!("{cctx}: autoIncrement is only supported on a primaryKey column"));
    }
    Ok(c)
}

fn enum_check_name(d: Dialect, table: &str, col: &str, ctx: &str) -> Result<String, String> {
    quote_ident(d, &format!("chk_{table}_{col}"), ctx)
}

fn column_sql(d: Dialect, c: &Column) -> String {
    let mut s = String::new();
    if c.auto_increment {
        // Each driver spells an auto-numbered primary key differently.
        let ty = match (d, c.ty.as_str()) {
            (Dialect::Sqlite, _) => "INTEGER PRIMARY KEY AUTOINCREMENT".to_string(),
            (Dialect::Postgres, "INTEGER") => "SERIAL PRIMARY KEY".to_string(),
            (Dialect::Postgres, _) => "BIGSERIAL PRIMARY KEY".to_string(),
            (Dialect::Mysql, t) => format!("{t} AUTO_INCREMENT PRIMARY KEY"),
        };
        s.push_str(&format!("{} {}", c.name, ty));
    } else {
        s.push_str(&format!("{} {}", c.name, c.ty));
        if c.primary_key {
            s.push_str(" PRIMARY KEY");
        }
    }
    if c.not_null && !c.primary_key {
        s.push_str(" NOT NULL");
    }
    if c.unique && !c.primary_key {
        s.push_str(" UNIQUE");
    }
    if let Some(def) = &c.default {
        s.push_str(&format!(" DEFAULT {def}"));
    }
    if let Some((t, col)) = &c.references {
        s.push_str(&format!(" REFERENCES {t}({col})"));
    }
    if let Some((n, expr)) = &c.check {
        s.push_str(&format!(" CONSTRAINT {n} CHECK ({expr})"));
    }
    s
}

fn columns_of(d: Dialect, spec: &Value, ctx: &str) -> Result<Vec<Column>, String> {
    let list = get(spec, "columns")
        .and_then(|c| c.as_sequence())
        .ok_or_else(|| format!("{ctx}: 'columns' must be a list"))?;
    if list.is_empty() {
        return Err(format!("{ctx}: 'columns' must not be empty"));
    }
    let table = get_str(spec, "tableName", ctx)?;
    list.iter().map(|c| parse_column(d, &table, c, ctx)).collect()
}

fn name_list(d: Dialect, spec: &Value, key: &str, ctx: &str) -> Result<Vec<String>, String> {
    let list = get(spec, key)
        .and_then(|c| c.as_sequence())
        .ok_or_else(|| format!("{ctx}: '{key}' must be a list of names"))?;
    if list.is_empty() {
        return Err(format!("{ctx}: '{key}' must not be empty"));
    }
    list.iter()
        .map(|v| {
            let n = scalar_string(v).ok_or_else(|| format!("{ctx}: '{key}' entries must be names"))?;
            quote_ident(d, &n, ctx)
        })
        .collect()
}

/// Translate one change (`createTable: {...}`) into SQL statements for the dialect.
fn change_to_sql(d: Dialect, change: &Value, ctx: &str) -> Result<Vec<String>, String> {
    let map = change.as_mapping().filter(|m| m.len() == 1)
        .ok_or_else(|| format!("{ctx}: each change must be a mapping with exactly one change type"))?;
    let (kind, spec) = map.iter().next().unwrap();
    let kind = kind.as_str().unwrap_or("");
    let ctx = format!("{ctx}, {kind}");
    let table = |spec: &Value, key: &str| -> Result<String, String> {
        quote_ident(d, &get_str(spec, key, &ctx)?, &ctx)
    };
    match kind {
        "createTable" => {
            check_keys(spec, &["tableName", "columns"], &ctx)?;
            let t = table(spec, "tableName")?;
            let cols = columns_of(d, spec, &ctx)?;
            let pk_count = cols.iter().filter(|c| c.primary_key).count();
            if pk_count > 1 {
                return Err(format!("{ctx}: more than one primaryKey column (composite keys: use a 'sql:' change)"));
            }
            let body: Vec<String> = cols.iter().map(|c| column_sql(d, c)).collect();
            Ok(vec![format!("CREATE TABLE {t} ({})", body.join(", "))])
        }
        "dropTable" => {
            check_keys(spec, &["tableName"], &ctx)?;
            Ok(vec![format!("DROP TABLE {}", table(spec, "tableName")?)])
        }
        "renameTable" => {
            check_keys(spec, &["oldTableName", "newTableName"], &ctx)?;
            Ok(vec![format!("ALTER TABLE {} RENAME TO {}", table(spec, "oldTableName")?, table(spec, "newTableName")?)])
        }
        "addColumn" => {
            check_keys(spec, &["tableName", "columns"], &ctx)?;
            let t = table(spec, "tableName")?;
            let cols = columns_of(d, spec, &ctx)?;
            let mut out = Vec::new();
            for c in &cols {
                if c.auto_increment || c.primary_key {
                    return Err(format!("{ctx}: addColumn cannot add a primaryKey/autoIncrement column"));
                }
                if d == Dialect::Sqlite && c.not_null && c.default.is_none() {
                    return Err(format!("{ctx}: SQLite cannot add a NOT NULL column without a defaultValue"));
                }
                if d == Dialect::Sqlite && c.unique {
                    return Err(format!("{ctx}: SQLite cannot add a UNIQUE column; use createIndex with unique: true"));
                }
                out.push(format!("ALTER TABLE {t} ADD COLUMN {}", column_sql(d, c)));
            }
            Ok(out)
        }
        "modifyEnumCheck" => {
            check_keys(spec, &["tableName", "columnName", "type"], &ctx)?;
            let t = table(spec, "tableName")?;
            let (tn, cn) = (get_str(spec, "tableName", &ctx)?, get_str(spec, "columnName", &ctx)?);
            let col = quote_ident(d, &cn, &ctx)?;
            let ty = get_str(spec, "type", &ctx)?;
            let (_, vars) = enum_variants(&ty, &ctx)?
                .ok_or_else(|| format!("{ctx}: 'type' must be enum(Name)"))?;
            let name = enum_check_name(d, &tn, &cn, &ctx)?;
            let add = format!("ALTER TABLE {t} ADD CONSTRAINT {name} CHECK ({})", enum_check_expr(d, &col, &vars));
            match d {
                Dialect::Postgres => Ok(vec![format!("ALTER TABLE {t} DROP CONSTRAINT {name}"), add]),
                Dialect::Mysql => Ok(vec![format!("ALTER TABLE {t} DROP CHECK {name}"), add]),
                Dialect::Sqlite => Err(format!("{ctx}: SQLite cannot alter a CHECK constraint; use enumCheck: false on the column or rebuild the table with 'sql:'")),
            }
        }
        "dropColumn" => {
            check_keys(spec, &["tableName", "columnName"], &ctx)?;
            Ok(vec![format!("ALTER TABLE {} DROP COLUMN {}", table(spec, "tableName")?, table(spec, "columnName")?)])
        }
        "renameColumn" => {
            check_keys(spec, &["tableName", "oldColumnName", "newColumnName"], &ctx)?;
            Ok(vec![format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                table(spec, "tableName")?, table(spec, "oldColumnName")?, table(spec, "newColumnName")?
            )])
        }
        "createIndex" => {
            check_keys(spec, &["tableName", "indexName", "columns", "unique"], &ctx)?;
            let unique = if get_bool(spec, "unique", &ctx)? { "UNIQUE " } else { "" };
            let cols = name_list(d, spec, "columns", &ctx)?;
            Ok(vec![format!(
                "CREATE {unique}INDEX {} ON {} ({})",
                table(spec, "indexName")?, table(spec, "tableName")?, cols.join(", ")
            )])
        }
        "dropIndex" => {
            check_keys(spec, &["tableName", "indexName"], &ctx)?;
            let idx = table(spec, "indexName")?;
            Ok(vec![if d == Dialect::Mysql {
                format!("DROP INDEX {idx} ON {}", table(spec, "tableName")?)
            } else {
                format!("DROP INDEX {idx}")
            }])
        }
        "insert" => {
            check_keys(spec, &["tableName", "values"], &ctx)?;
            let t = table(spec, "tableName")?;
            let vals = get(spec, "values").and_then(|v| v.as_mapping())
                .ok_or_else(|| format!("{ctx}: 'values' must be a mapping of column: value"))?;
            if vals.is_empty() {
                return Err(format!("{ctx}: 'values' must not be empty"));
            }
            let mut names = Vec::new();
            let mut lits = Vec::new();
            for (k, v) in vals {
                names.push(quote_ident(d, k.as_str().unwrap_or(""), &ctx)?);
                lits.push(sql_literal(v, &ctx)?);
            }
            Ok(vec![format!("INSERT INTO {t} ({}) VALUES ({})", names.join(", "), lits.join(", "))])
        }
        "sql" => {
            // Escape hatch. `dbms:` limits it to one driver; a change set can carry several
            // 'sql' changes with different dbms values for dialect-specific statements.
            let (text, only): (Value, Option<String>) = if spec.is_mapping() {
                check_keys(spec, &["statements", "dbms"], &ctx)?;
                (
                    get(spec, "statements").cloned().ok_or_else(|| format!("{ctx}: missing 'statements'"))?,
                    get(spec, "dbms").and_then(scalar_string),
                )
            } else {
                (spec.clone(), None)
            };
            if let Some(only) = only {
                if Dialect::from_driver(&only).is_err() {
                    return Err(format!("{ctx}: unknown dbms '{only}'"));
                }
                if only != d.name() {
                    return Ok(vec![]);
                }
            }
            let stmts: Vec<String> = match &text {
                Value::String(s) => vec![s.clone()],
                Value::Sequence(items) => items
                    .iter()
                    .map(|i| scalar_string(i).ok_or_else(|| format!("{ctx}: statements must be strings")))
                    .collect::<Result<_, _>>()?,
                _ => return Err(format!("{ctx}: expected a SQL string or a list of SQL strings")),
            };
            let stmts: Vec<String> = stmts.into_iter().map(|s| s.trim().trim_end_matches(';').trim().to_string()).collect();
            if stmts.iter().any(|s| s.is_empty()) {
                return Err(format!("{ctx}: empty SQL statement"));
            }
            if stmts.iter().any(|s| s.contains(';')) {
                return Err(format!("{ctx}: one statement per entry (no ';' inside); use a list for several statements"));
            }
            Ok(stmts)
        }
        other => Err(format!(
            "{ctx}: unsupported change type '{other}' (supported: createTable, dropTable, renameTable, addColumn, dropColumn, renameColumn, modifyEnumCheck, createIndex, dropIndex, insert, sql)"
        )),
    }
}

fn mapping1(key: &str, spec: Vec<(&str, Value)>) -> Value {
    let mut inner = serde_yaml::Mapping::new();
    for (k, v) in spec {
        inner.insert(Value::String(k.to_string()), v);
    }
    let mut outer = serde_yaml::Mapping::new();
    outer.insert(Value::String(key.to_string()), Value::Mapping(inner));
    Value::Mapping(outer)
}

/// The change that undoes `change`, or the reason there is none.
fn invert_change(d: Dialect, change: &Value) -> Result<Vec<Value>, String> {
    let s = |v: &Value, k: &str| get(v, k).cloned().unwrap_or(Value::Null);
    let map = change.as_mapping().and_then(|m| m.iter().next());
    let Some((kind, spec)) = map else { return Err("malformed change".into()) };
    match kind.as_str().unwrap_or("") {
        "createTable" => Ok(vec![mapping1("dropTable", vec![("tableName", s(spec, "tableName"))])]),
        "addColumn" => {
            let cols = get(spec, "columns").and_then(|c| c.as_sequence()).cloned().unwrap_or_default();
            Ok(cols.iter().rev().map(|c| {
                let name = get(c, "column").map(|c| s(c, "name")).unwrap_or(Value::Null);
                mapping1("dropColumn", vec![("tableName", s(spec, "tableName")), ("columnName", name)])
            }).collect())
        }
        "createIndex" => Ok(vec![mapping1("dropIndex", vec![("tableName", s(spec, "tableName")), ("indexName", s(spec, "indexName"))])]),
        "renameTable" => Ok(vec![mapping1("renameTable", vec![("oldTableName", s(spec, "newTableName")), ("newTableName", s(spec, "oldTableName"))])]),
        "renameColumn" => Ok(vec![mapping1("renameColumn", vec![
            ("tableName", s(spec, "tableName")),
            ("oldColumnName", s(spec, "newColumnName")),
            ("newColumnName", s(spec, "oldColumnName")),
        ])]),
        "sql" => {
            // A 'sql' change for another dbms does nothing here, so there is nothing to undo.
            if change_to_sql(d, change, "sql")?.is_empty() { Ok(vec![]) } else { Err("a 'sql' change cannot be undone automatically".into()) }
        }
        k => Err(format!("'{k}' cannot be undone automatically")),
    }
}

fn rollback_of(d: Dialect, cs: &Value, changes: &[Value], ctx: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    if let Some(rb) = get(cs, "rollback") {
        let list = rb.as_sequence().ok_or_else(|| format!("{ctx}: 'rollback' must be a list of changes"))?;
        for c in list {
            out.extend(change_to_sql(d, c, &format!("{ctx}, rollback"))?);
        }
        return Ok(out);
    }
    for c in changes.iter().rev() {
        for inv in invert_change(d, c)? {
            out.extend(change_to_sql(d, &inv, ctx)?);
        }
    }
    Ok(out)
}

fn checksum_of(cs: &Value) -> String {
    // 'rollback' and 'tag' do not change what the changeSet did, so adding one later must not
    // invalidate the checksum of an already applied changeSet.
    let mut cs = cs.clone();
    if let Some(m) = cs.as_mapping_mut() {
        m.remove(Value::String("rollback".into()));
        m.remove(Value::String("tag".into()));
    }
    let cs = &cs;
    // serde_yaml keeps mapping order, so the checksum follows the file's text order but not
    // its formatting (whitespace/comments/quoting do not change it).
    let canonical = serde_json::to_string(cs).unwrap_or_default();
    let digest = Sha256::digest(canonical.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_changeset(d: Dialect, cs: &Value, file: &str) -> Result<Option<ChangeSet>, String> {
    let id = get_str(cs, "id", &format!("{file}: changeSet"))?;
    let author = get_str(cs, "author", &format!("{file}: changeSet '{id}'"))?;
    let ctx = format!("{file}: changeSet '{id}' by '{author}'");
    check_keys(cs, &["id", "author", "changes", "runOnChange", "runAlways", "dbms", "comment", "rollback", "tag"], &ctx)?;
    if id.contains('\u{1f}') || author.contains('\u{1f}') || id.is_empty() || author.is_empty() {
        return Err(format!("{ctx}: id and author must be non-empty text"));
    }
    if let Some(dbms) = get(cs, "dbms").and_then(scalar_string) {
        let wanted: Vec<&str> = dbms.split(',').map(str::trim).collect();
        for w in &wanted {
            Dialect::from_driver(w).map_err(|e| format!("{ctx}: {e}"))?;
        }
        if !wanted.contains(&d.name()) {
            return Ok(None); // not for this driver; neither applied nor recorded
        }
    }
    let changes = get(cs, "changes").and_then(|c| c.as_sequence())
        .ok_or_else(|| format!("{ctx}: 'changes' must be a list"))?;
    if changes.is_empty() {
        return Err(format!("{ctx}: 'changes' must not be empty"));
    }
    let mut stmts = Vec::new();
    for c in changes {
        stmts.extend(change_to_sql(d, c, &ctx)?);
    }
    // An invalid explicit rollback block is a definition error; an inherently irreversible
    // change only becomes an error when a rollback is requested.
    let rollback = match rollback_of(d, cs, changes, &ctx) {
        Err(e) if get(cs, "rollback").is_some() => return Err(e),
        r => r.map_err(|e| format!("{ctx}: cannot be rolled back: {e} (add a 'rollback:' block)")),
    };
    let tag = match get(cs, "tag") {
        None => None,
        Some(v) => {
            let t = scalar_string(v).filter(|t| !t.is_empty() && !t.contains(['\u{1d}', '\u{1e}', '\u{1f}']));
            Some(t.ok_or_else(|| format!("{ctx}: 'tag' must be non-empty text"))?)
        }
    };
    Ok(Some(ChangeSet {
        rollback,
        tag,
        id,
        author,
        file: file.to_string(),
        checksum: checksum_of(cs),
        run_on_change: get_bool(cs, "runOnChange", &ctx)?,
        run_always: get_bool(cs, "runAlways", &ctx)?,
        stmts,
    }))
}

fn load_file(
    d: Dialect,
    root_dir: &Path,
    path: &Path,
    stack: &mut Vec<PathBuf>,
    out: &mut Vec<ChangeSet>,
) -> Result<(), String> {
    let canon = path.canonicalize().map_err(|e| format!("cannot read changelog {}: {e}", path.display()))?;
    if stack.contains(&canon) {
        return Err(format!("changelog include cycle at {}", path.display()));
    }
    stack.push(canon.clone());
    let rel = canon
        .strip_prefix(root_dir.canonicalize().unwrap_or_else(|_| root_dir.to_path_buf()))
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| path.to_string_lossy().into_owned());
    let text = std::fs::read_to_string(&canon).map_err(|e| format!("cannot read changelog {}: {e}", path.display()))?;
    let is_xml = canon.extension().is_some_and(|e| e.eq_ignore_ascii_case("xml"));
    let doc: Value = if is_xml {
        crate::migrate_xml::parse_changelog(&text).map_err(|e| format!("{rel}: invalid XML: {e}"))?
    } else {
        serde_yaml::from_str(&text).map_err(|e| format!("{rel}: invalid YAML: {e}"))?
    };
    check_keys(&doc, &["databaseChangeLog"], &rel)?;
    let items = get(&doc, "databaseChangeLog").and_then(|v| v.as_sequence())
        .ok_or_else(|| format!("{rel}: 'databaseChangeLog' must be a list"))?;
    let dir = canon.parent().unwrap_or(Path::new(".")).to_path_buf();
    for item in items {
        let m = item.as_mapping().filter(|m| m.len() == 1)
            .ok_or_else(|| format!("{rel}: each databaseChangeLog entry must be 'changeSet:' or 'include:'"))?;
        let (k, v) = m.iter().next().unwrap();
        match k.as_str().unwrap_or("") {
            "changeSet" => {
                if let Some(cs) = parse_changeset(d, v, &rel)? {
                    out.push(cs);
                }
            }
            "include" => {
                check_keys(v, &["file"], &format!("{rel}: include"))?;
                let f = get_str(v, "file", &format!("{rel}: include"))?;
                load_file(d, root_dir, &dir.join(f), stack, out)?;
            }
            other => return Err(format!("{rel}: unknown entry '{other}' (expected changeSet or include)")),
        }
    }
    stack.pop();
    Ok(())
}

/// Load a changelog (following `include:`s) and translate it for `dialect`.
/// Hard errors: duplicate id+author, anything unparseable or unsupported.
pub fn load_changelog(
    path: &Path,
    dialect: Dialect,
    enums: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<Vec<ChangeSet>, String> {
    ENUMS.with(|e| *e.borrow_mut() = enums.clone());
    let root_dir = path.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    load_file(dialect, root_dir, path, &mut Vec::new(), &mut out)?;
    let mut seen = HashSet::new();
    for cs in &out {
        if !seen.insert((cs.id.clone(), cs.author.clone())) {
            return Err(format!("duplicate changeSet id '{}' by '{}' (in {})", cs.id, cs.author, cs.file));
        }
    }
    let mut tags = HashSet::new();
    for cs in &out {
        if let Some(t) = &cs.tag {
            if !tags.insert(t.clone()) {
                return Err(format!("duplicate tag '{t}' (changeSet '{}' by '{}')", cs.id, cs.author));
            }
        }
    }
    if out.is_empty() {
        return Err(format!("changelog {} contains no changeSets for driver {}", path.display(), dialect.name()));
    }
    Ok(out)
}

/// Wire format read by `tinox_migrate_run` in runtime.c: change sets separated by 0x1d, fields by
/// 0x1f (id, author, file, checksum, flags, statements joined by 0x1e, rollback statements joined
/// by 0x1e, reason the changeSet is irreversible or empty, tag or empty).
pub fn serialize_payload(sets: &[ChangeSet]) -> String {
    let recs: Vec<String> = sets
        .iter()
        .map(|c| {
            let flags = format!("{}{}", if c.run_on_change { "C" } else { "" }, if c.run_always { "A" } else { "" });
            let (rb, why) = match &c.rollback {
                Ok(v) => (v.join("\u{1e}"), String::new()),
                Err(e) => (String::new(), e.clone()),
            };
            [c.id.as_str(), c.author.as_str(), c.file.as_str(), c.checksum.as_str(), flags.as_str(), &c.stmts.join("\u{1e}"),
             rb.as_str(), why.as_str(), c.tag.as_deref().unwrap_or("")]
                .join("\u{1f}")
        })
        .collect();
    recs.join("\u{1d}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(d: Dialect, yaml: &str) -> Result<Option<ChangeSet>, String> {
        let v: Value = serde_yaml::from_str(yaml).unwrap();
        parse_changeset(d, &v, "t.yaml")
    }

    #[test]
    fn create_table_per_dialect() {
        let y = "id: 1\nauthor: a\nchanges:\n  - createTable:\n      tableName: person\n      columns:\n        - column: {name: id, type: bigint, autoIncrement: true, constraints: {primaryKey: true}}\n        - column: {name: name, type: varchar(10), constraints: {nullable: false}}\n        - column: {name: active, type: boolean, defaultValue: true}\n";
        let sq = cs(Dialect::Sqlite, y).unwrap().unwrap();
        assert_eq!(sq.stmts[0], "CREATE TABLE \"person\" (\"id\" INTEGER PRIMARY KEY AUTOINCREMENT, \"name\" VARCHAR(10) NOT NULL, \"active\" INTEGER DEFAULT 1)");
        let pg = cs(Dialect::Postgres, y).unwrap().unwrap();
        assert_eq!(pg.stmts[0], "CREATE TABLE \"person\" (\"id\" BIGSERIAL PRIMARY KEY, \"name\" VARCHAR(10) NOT NULL, \"active\" BOOLEAN DEFAULT 1)");
        assert_eq!(sq.checksum, pg.checksum, "checksum must not depend on the driver");
    }

    #[test]
    fn enum_columns_and_modify() {
        ENUMS.with(|e| { e.borrow_mut().insert("Status".into(), vec!["Open".into(), "Done".into()]); });
        let y = "id: 1\nauthor: a\nchanges:\n  - createTable:\n      tableName: t\n      columns:\n        - column: {name: st, type: enum(Status), defaultValue: Open, constraints: {nullable: false}}\n        - column: {name: s2, type: enum(Status), enumCheck: false}\n";
        let pg = cs(Dialect::Postgres, y).unwrap().unwrap();
        assert_eq!(pg.stmts[0], "CREATE TABLE \"t\" (\"st\" VARCHAR(255) NOT NULL DEFAULT 'Open' CONSTRAINT \"chk_t_st\" CHECK (\"st\" IN ('Open', 'Done')), \"s2\" VARCHAR(255))");
        let bad = y.replace("defaultValue: Open", "defaultValue: Nope");
        assert!(cs(Dialect::Postgres, &bad).unwrap_err().contains("variant of enum Status"));
        assert!(cs(Dialect::Sqlite, &y.replace("Status", "Missing")).unwrap_err().contains("unknown Tinox enum"));
        let m = "id: 2\nauthor: a\nchanges:\n  - modifyEnumCheck: {tableName: t, columnName: st, type: enum(Status)}\n";
        assert_eq!(cs(Dialect::Postgres, m).unwrap().unwrap().stmts.len(), 2);
        assert!(cs(Dialect::Mysql, m).unwrap().unwrap().stmts[0].contains("DROP CHECK"));
        assert!(cs(Dialect::Sqlite, m).is_err());
    }

    #[test]
    fn unsupported_and_unsafe_are_hard_errors() {
        assert!(cs(Dialect::Sqlite, "id: 1\nauthor: a\nchanges:\n  - frobnicate: {}\n").is_err());
        assert!(cs(Dialect::Sqlite, "id: 1\nauthor: a\nchanges:\n  - dropTable: {tableName: \"x; DROP\"}\n").is_err());
        assert!(cs(Dialect::Sqlite, "id: 1\nauthor: a\nchanges:\n  - createTable: {tableName: t, columns: [{column: {name: a, type: weird}}]}\n").is_err());
        assert!(cs(Dialect::Sqlite, "id: 1\nauthor: a\nbogus: 1\nchanges:\n  - dropTable: {tableName: t}\n").is_err());
    }

    #[test]
    fn dbms_filter_and_checksum_changes() {
        let a = "id: 1\nauthor: a\ndbms: postgres\nchanges:\n  - dropTable: {tableName: t}\n";
        assert!(cs(Dialect::Sqlite, a).unwrap().is_none());
        assert!(cs(Dialect::Postgres, a).unwrap().is_some());
        let b = "id: 1\nauthor: a\nchanges:\n  - dropTable: {tableName: t}\n";
        let c = "id: 1\nauthor: a\nchanges:\n  - dropTable: {tableName: u}\n";
        assert_ne!(cs(Dialect::Sqlite, b).unwrap().unwrap().checksum, cs(Dialect::Sqlite, c).unwrap().unwrap().checksum);
    }

    #[test]
    fn rollback_derived_explicit_and_irreversible() {
        let y = "id: 1\nauthor: a\nchanges:\n  - createTable: {tableName: t, columns: [{column: {name: a, type: int}}]}\n  - createIndex: {tableName: t, indexName: i, columns: [a]}\n  - addColumn: {tableName: t, columns: [{column: {name: b, type: int}}, {column: {name: c, type: int}}]}\n";
        let r = cs(Dialect::Sqlite, y).unwrap().unwrap().rollback.unwrap();
        assert_eq!(r, vec![
            "ALTER TABLE \"t\" DROP COLUMN \"c\"", "ALTER TABLE \"t\" DROP COLUMN \"b\"",
            "DROP INDEX \"i\"", "DROP TABLE \"t\""]);
        let bad = "id: 1\nauthor: a\nchanges:\n  - dropTable: {tableName: t}\n";
        assert!(cs(Dialect::Sqlite, bad).unwrap().unwrap().rollback.is_err());
        let ok = format!("{bad}rollback:\n  - sql: CREATE TABLE t (a INTEGER)\n");
        let c = cs(Dialect::Sqlite, &ok).unwrap().unwrap();
        assert_eq!(c.rollback.unwrap(), vec!["CREATE TABLE t (a INTEGER)"]);
        assert_eq!(c.checksum, cs(Dialect::Sqlite, bad).unwrap().unwrap().checksum, "rollback is not part of the checksum");
        assert!(cs(Dialect::Sqlite, &format!("{bad}rollback: nope\n")).is_err());
    }
}
