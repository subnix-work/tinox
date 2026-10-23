//! Offline, additive schema generation. Existing YAML bytes are never serialized again.
use super::*;
use std::collections::BTreeMap;
use std::fs;
use tinox_parser::{Annotation, AnnotationArg, Decl, DeclKind, Literal, Type};

type Schema = BTreeMap<String, BTreeMap<String, Value>>;

fn object(entries: &[(&str, Value)]) -> Value {
    Value::Mapping(
        entries
            .iter()
            .map(|(k, v)| (Value::String((*k).into()), v.clone()))
            .collect(),
    )
}
fn string(s: &str) -> Value {
    Value::String(s.into())
}

fn annotation_name(
    annotations: &[Annotation],
    name: &str,
    default: &str,
) -> Result<String, String> {
    match annotations.iter().find(|a| a.name == name) {
        None => Ok(default.into()),
        Some(a) => match a.args.as_slice() {
            [] if name == "Column" => Ok(default.into()),
            [AnnotationArg::Literal(Literal::String(s))] => Ok(s.clone()),
            _ => Err(format!("@{name} requires one string argument")),
        },
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "tnx") {
            out.push(path);
        }
    }
    Ok(())
}
fn flatten<'a>(decls: &'a [Decl], out: &mut Vec<&'a Decl>) {
    for decl in decls {
        if let DeclKind::Namespace(ns) = &decl.node {
            flatten(&ns.decls, out);
        } else {
            out.push(decl);
        }
    }
}

fn entity_schema(root: &Path) -> Result<Schema, String> {
    let mut files = Vec::new();
    collect_files(&root.join("src"), &mut files)?;
    files.sort();
    let mut sources = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let tokens = tinox_lexer::Lexer::new(&text)
            .tokenize()
            .map_err(|e| format!("{}: {e:?}", path.display()))?;
        sources.push(
            tinox_parser::Parser::new(tokens)
                .parse()
                .map_err(|e| format!("{}: {e}", path.display()))?,
        );
    }
    let mut decls = Vec::new();
    for source in &sources {
        flatten(&source.decls, &mut decls);
    }
    let enums: HashSet<_> = decls
        .iter()
        .filter_map(|d| match &d.node {
            DeclKind::Enum(e) if e.variants.iter().all(|v| v.args.is_empty()) => {
                Some(e.name.as_str())
            }
            _ => None,
        })
        .collect();
    let mut schema = Schema::new();
    for decl in &decls {
        let DeclKind::Class(class) = &decl.node else {
            continue;
        };
        if !class
            .annotations
            .iter()
            .any(|a| a.name == "Entity" || a.name == "Table")
        {
            continue;
        }
        if class.extends.is_some() || !class.type_params.is_empty() {
            return Err(format!(
                "entity {}: inheritance and generic entities are unsupported",
                class.name
            ));
        }
        let table = annotation_name(&class.annotations, "Table", &class.name.to_lowercase())?;
        let mut columns = BTreeMap::new();
        for field in &class.fields {
            let name = annotation_name(&field.annotations, "Column", &field.name)?;
            let has = |ann: &str| field.annotations.iter().any(|a| a.name == ann);
            let ty = match &field.field_type {
                Type::Int8 | Type::Int16 | Type::Int32 | Type::UInt8 | Type::UInt16 => "int",
                Type::Int64 | Type::UInt32 => "bigint",
                Type::Float32 => "float",
                Type::Float64 => "double",
                Type::Bool => "boolean",
                Type::String => "text",
                Type::Char => "char(1)",
                Type::Named(n) if enums.contains(n.as_str()) => "text",
                other => {
                    return Err(format!(
                        "entity {}.{}: unsupported column type {other:?}",
                        class.name, field.name
                    ))
                }
            };
            if has("GeneratedValue") && (!has("Id") || !matches!(ty, "int" | "bigint")) {
                return Err(format!(
                    "entity {}.{}: @GeneratedValue requires an integer @Id",
                    class.name, field.name
                ));
            }
            let col = object(&[
                ("name", string(&name)),
                ("type", string(ty)),
                ("autoIncrement", Value::Bool(has("GeneratedValue"))),
                (
                    "constraints",
                    object(&[
                        ("primaryKey", Value::Bool(has("Id"))),
                        ("nullable", Value::Bool(!has("NotNull"))),
                    ]),
                ),
            ]);
            if columns.insert(name.clone(), col).is_some() {
                return Err(format!("duplicate entity column {table}.{name}"));
            }
        }
        if columns
            .values()
            .filter(|c| get_bool(&c["constraints"], "primaryKey", "entity").unwrap_or(false))
            .count()
            > 1
        {
            return Err(format!(
                "entity {table}: composite primary keys are unsupported"
            ));
        }
        if schema.insert(table.clone(), columns).is_some() {
            return Err(format!("duplicate entity table {table}"));
        }
    }
    if schema.is_empty() {
        return Err("no @Entity/@Table classes found under src/".into());
    }
    Ok(schema)
}

fn columns(spec: &Value) -> impl Iterator<Item = &Value> {
    spec["columns"]
        .as_sequence()
        .into_iter()
        .flatten()
        .map(|v| &v["column"])
}

fn replay(sets: &[ChangeSet]) -> Result<Schema, String> {
    let mut schema = Schema::new();
    for set in sets {
        for change in &set.changes {
            let (kind, spec) = change.as_mapping().unwrap().iter().next().unwrap();
            let kind = kind.as_str().unwrap();
            let table = || get_str(spec, "tableName", &set.id);
            match kind {
                "createTable" => {
                    let name = table()?;
                    let cols = columns(spec).map(|c| Ok((get_str(c, "name", &set.id)?, c.clone()))).collect::<Result<_, String>>()?;
                    if schema.insert(name.clone(), cols).is_some() { return Err(format!("changeSet {}: duplicate table {name}", set.id)); }
                }
                "dropTable" => { schema.remove(&table()?); }
                "renameTable" => {
                    let old = get_str(spec, "oldTableName", &set.id)?;
                    let new = get_str(spec, "newTableName", &set.id)?;
                    let cols = schema.remove(&old).ok_or_else(|| format!("unknown table {old}"))?;
                    if schema.insert(new.clone(), cols).is_some() { return Err(format!("duplicate table {new}")); }
                }
                "addColumn" | "dropColumn" | "renameColumn" => {
                    let name = table()?;
                    let cols = schema.get_mut(&name).ok_or_else(|| format!("unknown table {name}"))?;
                    match kind {
                        "addColumn" => for col in columns(spec) {
                            let name = get_str(col, "name", &set.id)?;
                            if cols.insert(name.clone(), col.clone()).is_some() { return Err(format!("duplicate column {name}")); }
                        },
                        "dropColumn" => { cols.remove(&get_str(spec, "columnName", &set.id)?); }
                        _ => {
                            let old = get_str(spec, "oldColumnName", &set.id)?;
                            let new = get_str(spec, "newColumnName", &set.id)?;
                            let mut col = cols.remove(&old).ok_or_else(|| format!("unknown column {old}"))?;
                            col["name"] = string(&new);
                            if cols.insert(new.clone(), col).is_some() { return Err(format!("duplicate column {new}")); }
                        }
                    }
                }
                "createIndex" | "dropIndex" | "insert" => {}
                _ => return Err(format!("changeSet {}: cannot infer schema from '{kind}'; maintain this changelog manually", set.id)),
            }
        }
    }
    Ok(schema)
}

fn equivalent(d: Dialect, table: &str, old: &Value, new: &Value) -> Result<bool, String> {
    let wrap = |c: &Value| object(&[("column", c.clone())]);
    let a = parse_column(d, table, &wrap(old), "existing schema")?;
    let b = parse_column(d, table, &wrap(new), "entity model")?;
    // Defaults, indexes and references are independently maintained by hand.
    Ok(a.ty == b.ty
        && a.auto_increment == b.auto_increment
        && a.primary_key == b.primary_key
        && (a.not_null || a.primary_key) == (b.not_null || b.primary_key))
}

pub fn generate(root: &Path, path: &Path, d: Dialect) -> Result<String, String> {
    let model = entity_schema(root)?;
    ENUMS.with(|e| *e.borrow_mut() = crate::project_enums(root));
    let original = match fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut sets = Vec::new();
    if original.is_some() {
        load_file(
            d,
            path.parent().unwrap_or(Path::new(".")),
            path,
            &mut Vec::new(),
            &mut sets,
        )?;
    }
    let mut ids = HashSet::new();
    for cs in &sets {
        if !ids.insert((cs.id.clone(), cs.author.clone())) {
            return Err(format!("duplicate changeSet {} by {}", cs.id, cs.author));
        }
    }
    let existing = replay(&sets)?;
    let mut changes = Vec::new();
    for (table, cols) in &model {
        quote_ident(d, table, "entity")?;
        if let Some(old) = existing.get(table) {
            for (name, col) in cols {
                if let Some(previous) = old.get(name) {
                    if !equivalent(d, table, previous, col)? {
                        return Err(format!(
                            "{table}.{name}: type or constraint changed; add a manual changeSet"
                        ));
                    }
                } else {
                    changes.push(object(&[(
                        "addColumn",
                        object(&[
                            ("tableName", string(table)),
                            (
                                "columns",
                                Value::Sequence(vec![object(&[("column", col.clone())])]),
                            ),
                        ]),
                    )]));
                }
            }
            for name in old.keys() {
                if !cols.contains_key(name) {
                    return Err(format!(
                        "{table}.{name}: column removed or renamed; add a manual changeSet"
                    ));
                }
            }
        } else {
            changes.push(object(&[(
                "createTable",
                object(&[
                    ("tableName", string(table)),
                    (
                        "columns",
                        Value::Sequence(
                            cols.values()
                                .map(|c| object(&[("column", c.clone())]))
                                .collect(),
                        ),
                    ),
                ]),
            )]));
        }
    }
    // Tables unrelated to entities may coexist in the changelog.
    if changes.is_empty() {
        return Ok("Entity schema is up to date; no changeSet generated".into());
    }
    let mut n = 1;
    while ids.contains(&(format!("entity-{}-{n}", d.name()), "tinox".into())) {
        n += 1;
    }
    let id = format!("entity-{}-{n}", d.name());
    let cs = object(&[
        ("id", string(&id)),
        ("author", string("tinox")),
        ("dbms", string(d.name())),
        ("changes", Value::Sequence(changes)),
    ]);
    parse_changeset(d, &cs, "generated")?; // Validate everything before writing, including dialect restrictions.
    let entry = object(&[("changeSet", cs)]);
    let suffix = serde_yaml::to_string(&vec![entry.clone()]).map_err(|e| e.to_string())?;
    let indent = original
        .as_ref()
        .and_then(|s| {
            s.lines()
                .filter(|l| l.trim_start().starts_with("- "))
                .map(|l| l.len() - l.trim_start().len())
                .min()
        })
        .unwrap_or(2);
    let padding = " ".repeat(indent);
    let text = match &original {
        None => format!(
            "databaseChangeLog:\n{}",
            suffix
                .lines()
                .map(|l| format!("{padding}{l}\n"))
                .collect::<String>()
        ),
        Some(before) => {
            let result = format!(
                "{before}\n{}",
                suffix
                    .lines()
                    .map(|l| format!("{padding}{l}\n"))
                    .collect::<String>()
            );
            let old: Value = serde_yaml::from_str(before).map_err(|e| e.to_string())?;
            let mut expected = old.clone();
            expected["databaseChangeLog"]
                .as_sequence_mut()
                .unwrap()
                .push(entry);
            let actual: Value = serde_yaml::from_str(&result).map_err(|_| "cannot append to this YAML layout; use a block-style databaseChangeLog list without a document end marker".to_string())?;
            if actual != expected {
                return Err("cannot append safely to this YAML layout; use a block-style databaseChangeLog list".into());
            }
            result
        }
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    // A sibling temporary file avoids partially written changelogs on failure.
    let temp = path.with_extension(format!("yaml.{}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        use std::io::Write;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        if fs::read_to_string(path).ok() != original {
            return Err("changelog changed during generation; retry".into());
        }
        fs::rename(&temp, path).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(&temp);
    result?;
    Ok(format!(
        "{}: generated changeSet {id} by tinox",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Project(PathBuf);
    impl Project {
        fn new() -> Self {
            let p = Self(std::env::temp_dir().join(format!(
                "tinox-generate-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )));
            fs::create_dir_all(p.0.join("src/nested")).unwrap();
            p.model("");
            p
        }
        fn model(&self, extra: &str) {
            fs::write(
                self.0.join("src/nested/Person.tnx"),
                format!(
                    r#"
namespace demo {{
@Entity
@Table("people")
class Person {{
    @Id @GeneratedValue
    id: Int64;
    @Column("display_name") @NotNull
    name: String;
    @Column
    state: State;
    {extra}
}}
}}
"#
                ),
            )
            .unwrap();
            fs::write(
                self.0.join("src/State.tnx"),
                "namespace demo { enum State { ACTIVE, INACTIVE } }",
            )
            .unwrap();
        }
        fn path(&self) -> PathBuf {
            self.0.join("migrations/changelog.yaml")
        }
        fn generate(&self, d: Dialect) -> Result<String, String> {
            generate(&self.0, &self.path(), d)
        }
    }
    impl Drop for Project {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn initial_incremental_and_idempotent_for_all_drivers() {
        for d in [Dialect::Sqlite, Dialect::Postgres, Dialect::Mysql] {
            let p = Project::new();
            p.generate(d).unwrap();
            let original = fs::read(p.path()).unwrap();
            let initial = load_changelog(&p.path(), d, &BTreeMap::new()).unwrap();
            assert_eq!(initial.len(), 1);
            assert!(initial[0].stmts[0].contains("display_name"));
            assert!(initial[0].stmts[0].contains("state"));
            assert!(initial[0].stmts[0].contains("TEXT"));
            p.generate(d).unwrap();
            assert_eq!(fs::read(p.path()).unwrap(), original);
            p.model("active: Bool;");
            p.generate(d).unwrap();
            assert!(fs::read(p.path()).unwrap().starts_with(&original));
            let next = load_changelog(&p.path(), d, &BTreeMap::new()).unwrap();
            assert_eq!(next.len(), 2);
            assert_eq!(next[0].checksum, initial[0].checksum);
            assert!(next[1].stmts[0].starts_with("ALTER TABLE"));
            let bytes = fs::read(p.path()).unwrap();
            p.generate(d).unwrap();
            assert_eq!(fs::read(p.path()).unwrap(), bytes);
        }
    }

    #[test]
    fn includes_preserve_history() {
        let p = Project::new();
        p.generate(Dialect::Postgres).unwrap();
        let included = p.path().with_file_name("initial.yaml");
        fs::rename(p.path(), &included).unwrap();
        let bytes = fs::read(&included).unwrap();
        fs::write(
            p.path(),
            "# keep this comment\ndatabaseChangeLog:\n- include: { file: initial.yaml }\n",
        )
        .unwrap();
        p.model("email: String;");
        p.generate(Dialect::Postgres).unwrap();
        assert_eq!(fs::read(&included).unwrap(), bytes);
        assert!(fs::read_to_string(p.path())
            .unwrap()
            .starts_with("# keep this comment\n"));
        assert_eq!(
            load_changelog(&p.path(), Dialect::Postgres, &BTreeMap::new()).unwrap().len(),
            2
        );
    }

    #[test]
    fn drivers_have_independent_schemas_and_ids() {
        let p = Project::new();
        p.generate(Dialect::Sqlite).unwrap();
        p.generate(Dialect::Postgres).unwrap();
        let sqlite = load_changelog(&p.path(), Dialect::Sqlite, &BTreeMap::new()).unwrap();
        let postgres = load_changelog(&p.path(), Dialect::Postgres, &BTreeMap::new()).unwrap();
        assert_eq!(sqlite.len(), 1);
        assert_eq!(postgres.len(), 1);
        assert_ne!(sqlite[0].id, postgres[0].id);
        let before = fs::read(p.path()).unwrap();
        p.generate(Dialect::Sqlite).unwrap();
        p.generate(Dialect::Postgres).unwrap();
        assert_eq!(fs::read(p.path()).unwrap(), before);
    }

    #[test]
    fn manual_rename_is_replayed() {
        let p = Project::new();
        p.generate(Dialect::Postgres).unwrap();
        use std::io::Write;
        fs::OpenOptions::new().append(true).open(p.path()).unwrap().write_all(
            b"  - changeSet:\n      id: rename\n      author: manual\n      changes:\n        - renameColumn: {tableName: people, oldColumnName: display_name, newColumnName: name}\n"
        ).unwrap();
        let source = p.0.join("src/nested/Person.tnx");
        let model = fs::read_to_string(&source).unwrap();
        fs::write(
            source,
            model.replace("@Column(\"display_name\")", "@Column(\"name\")"),
        )
        .unwrap();
        let before = fs::read(p.path()).unwrap();
        p.generate(Dialect::Postgres).unwrap();
        assert_eq!(fs::read(p.path()).unwrap(), before);
    }

    #[test]
    fn unsafe_diffs_and_unsupported_models_do_not_write() {
        let p = Project::new();
        p.generate(Dialect::Sqlite).unwrap();
        let original = fs::read(p.path()).unwrap();
        for extra in [
            "@NotNull email: String;",
            "data: Any;",
            "@GeneratedValue counter: Int64;",
        ] {
            p.model(extra);
            assert!(p.generate(Dialect::Sqlite).is_err());
            assert_eq!(fs::read(p.path()).unwrap(), original);
        }
        p.model("");
        let source = p.0.join("src/nested/Person.tnx");
        let model = fs::read_to_string(&source).unwrap();
        fs::write(&source, model.replace("name: String", "name: Int64")).unwrap();
        assert!(p.generate(Dialect::Sqlite).unwrap_err().contains("changed"));
        assert_eq!(fs::read(p.path()).unwrap(), original);
        fs::write(&source, model.replace("@Column\n    state: State;", "")).unwrap();
        assert!(p.generate(Dialect::Sqlite).unwrap_err().contains("removed"));
        assert_eq!(fs::read(p.path()).unwrap(), original);
    }

    #[test]
    fn raw_sql_and_flow_layout_fail_without_writing() {
        let p = Project::new();
        fs::create_dir_all(p.path().parent().unwrap()).unwrap();
        for text in [
            "databaseChangeLog:\n  - changeSet: { id: manual, author: me, changes: [{sql: 'CREATE TABLE other (id INT)'}] }\n",
            "databaseChangeLog: []\n",
        ] {
            fs::write(p.path(), text).unwrap();
            assert!(p.generate(Dialect::Sqlite).is_err());
            assert_eq!(fs::read_to_string(p.path()).unwrap(), text);
        }
    }
}
