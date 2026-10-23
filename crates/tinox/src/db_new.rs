//! Offline changeSet scaffolding. Existing text is retained so comments and applied
//! changeSet checksums survive adding a skeleton.
use serde_yaml::Value;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn is_xml(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xml"))
}

fn parse(text: &str, xml: bool) -> Result<Value, String> {
    let doc = if xml {
        crate::migrate_xml::parse_changelog(text)?
    } else {
        serde_yaml::from_str::<Value>(text).map_err(|e| e.to_string())?
    };
    if doc.as_mapping().is_none_or(|m| m.len() != 1) || !doc["databaseChangeLog"].is_sequence() {
        return Err("expected a databaseChangeLog list".into());
    }
    Ok(doc)
}

fn read(path: &Path) -> Result<(String, Value), String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let doc = parse(&text, is_xml(path)).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((text, doc))
}

// Visit every include, including changeSets filtered out for the current driver.
// Do not require complete changes: earlier scaffolds may still be unfinished.
fn collect_ids(
    path: &Path,
    stack: &mut HashSet<PathBuf>,
    ids: &mut Vec<String>,
) -> Result<(), String> {
    let canon = path
        .canonicalize()
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !stack.insert(canon.clone()) {
        return Err(format!("changelog include cycle at {}", path.display()));
    }
    let (_, doc) = read(&canon)?;
    for item in doc["databaseChangeLog"].as_sequence().unwrap() {
        let map = item
            .as_mapping()
            .filter(|m| m.len() == 1)
            .ok_or_else(|| format!("{}: expected changeSet or include", path.display()))?;
        if let Some(cs) = map.get(Value::from("changeSet")) {
            let id = &cs["id"];
            let id = match id {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            }
            .ok_or_else(|| format!("{}: changeSet requires an id", path.display()))?;
            ids.push(id);
        } else if let Some(file) = item["include"]["file"].as_str() {
            collect_ids(
                &canon.parent().unwrap_or(Path::new(".")).join(file),
                stack,
                ids,
            )?;
        } else {
            return Err(format!(
                "{}: expected changeSet or include with a file",
                path.display()
            ));
        }
    }
    stack.remove(&canon);
    Ok(())
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Try text insertions and accept only one that parses to exactly the intended
/// document. This supports block/flow YAML and XML without rewriting existing text.
fn append(
    text: &str,
    doc: &Value,
    entry: &Value,
    xml_entry: &str,
    xml: bool,
) -> Result<String, String> {
    let mut expected = doc.clone();
    expected["databaseChangeLog"]
        .as_sequence_mut()
        .unwrap()
        .push(entry.clone());
    let matches = |s: &str| parse(s, xml).is_ok_and(|v| v == expected);
    if xml {
        for (pos, _) in text.match_indices("</databaseChangeLog") {
            let candidate = format!("{}\n{xml_entry}\n{}", &text[..pos], &text[pos..]);
            if matches(&candidate) {
                return Ok(candidate);
            }
        }
        for (pos, _) in text.match_indices("/>") {
            let candidate = format!(
                "{}>\n{xml_entry}\n</databaseChangeLog>{}",
                &text[..pos],
                &text[pos + 2..]
            );
            if matches(&candidate) {
                return Ok(candidate);
            }
        }
    } else {
        let block = serde_yaml::to_string(&vec![entry]).map_err(|e| e.to_string())?;
        let mut indents: Vec<usize> = text
            .lines()
            .map(|l| l.len() - l.trim_start_matches(' ').len())
            .collect();
        indents.extend([0, 2]);
        indents.sort_unstable();
        indents.dedup();
        let mut positions = vec![text.len()];
        positions.extend(
            text.match_indices("...")
                .filter(|(i, _)| *i == 0 || text.as_bytes()[i - 1] == b'\n')
                .map(|(i, _)| i),
        );
        for indent in indents {
            let block: String = block
                .lines()
                .map(|l| format!("{}{l}\n", " ".repeat(indent)))
                .collect();
            for &pos in &positions {
                let candidate = format!("{}\n{block}{}", &text[..pos], &text[pos..]);
                if matches(&candidate) {
                    return Ok(candidate);
                }
            }
        }
        let flow = serde_json::to_string(entry).map_err(|e| e.to_string())?;
        let comma = if doc["databaseChangeLog"].as_sequence().unwrap().is_empty() {
            ""
        } else {
            ","
        };
        for (pos, _) in text.match_indices(']').rev() {
            let candidate = format!("{}{comma}\n{flow}\n{}", &text[..pos], &text[pos..]);
            if matches(&candidate) {
                return Ok(candidate);
            }
        }
    }
    Err("cannot safely append to this changelog; use a block or flow databaseChangeLog list".into())
}

struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn write_new(path: &Path, text: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    if let Err(e) = file.write_all(text.as_bytes()) {
        let _ = fs::remove_file(path);
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(())
}

pub fn run(args: &[String]) -> Result<(), String> {
    let usage = "usage: tinox db new <description> [--include <file.yaml|file.xml>]";
    let mut description = None;
    let mut include = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--include" && include.is_none() {
            include = Some(
                it.next()
                    .filter(|s| !s.is_empty() && !s.starts_with('-'))
                    .ok_or(usage)?
                    .clone(),
            );
        } else if !arg.starts_with('-') && description.is_none() {
            description = Some(arg.clone());
        } else {
            return Err(usage.into());
        }
    }
    let description = description.filter(|s| !s.trim().is_empty()).ok_or(usage)?;
    let cfg =
        crate::read_database_config().ok_or("no [database] section with a driver in tinox.toml")?;
    let path = crate::changelog_path(&cfg);
    // Follow existing symlinks, matching the migration loader's include resolution
    // and keeping an atomic update from replacing the link itself.
    let path = if path.exists() {
        path.canonicalize()
            .map_err(|e| format!("cannot resolve {}: {e}", path.display()))?
    } else {
        path
    };
    let author = Command::new("git")
        .args(["config", "user.name"])
        .current_dir(&cfg.root)
        .output()
        .map_err(|e| format!("cannot read git author: {e}"))?;
    let author = if author.status.success() {
        String::from_utf8(author.stdout)
            .unwrap_or_default()
            .trim()
            .to_owned()
    } else {
        String::new()
    };
    if author.is_empty() {
        return Err("git user.name is not set; run git config user.name \"Your Name\"".into());
    }
    if author
        .chars()
        .chain(description.chars())
        .any(char::is_control)
    {
        return Err("author and description must not contain control characters".into());
    }
    let dir = path.parent().ok_or("changelog has no parent directory")?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let lock = path.with_extension("tinox-new.lock");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .map_err(|e| {
            format!(
                "cannot lock {}: {e} (another db new may be running)",
                path.display()
            )
        })?;
    let _lock = Lock(lock);
    let mut ids = Vec::new();
    let xml = is_xml(&path);
    let (text, doc) = if path.exists() {
        collect_ids(&path, &mut HashSet::new(), &mut ids)?;
        read(&path)?
    } else {
        let text = if xml {
            "<databaseChangeLog>\n</databaseChangeLog>\n"
        } else {
            "databaseChangeLog:\n"
        }
        .to_owned();
        let doc = parse(if xml { &text } else { "databaseChangeLog: []" }, xml)?;
        (text, doc)
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let last = ids
        .iter()
        .filter_map(|id| {
            id.split_once('-')
                .filter(|(n, _)| n.len() == 20)?
                .0
                .parse::<u128>()
                .ok()
        })
        .max()
        .unwrap_or(0);
    let stamp = now.max(last.checked_add(1).ok_or("changeSet id overflow")?);
    let slug = description
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let id = format!(
        "{stamp:020}-{}",
        if slug.is_empty() { "change" } else { &slug }
    );
    // Use explicit field order, which also defines the eventual checksum order.
    let skeleton: Value = serde_yaml::from_str(&format!(
        "changeSet:\n  id: {}\n  author: {}\n  comment: {}\n  changes: []\n",
        quote(&id),
        quote(&author),
        quote(&description)
    ))
    .map_err(|e| e.to_string())?;
    let xml_skeleton = format!("  <changeSet id=\"{}\" author=\"{}\">\n    <comment>{}</comment>\n    <!-- TODO: add changes here -->\n  </changeSet>", escape_xml(&id), escape_xml(&author), escape_xml(&description));
    let (entry, xml_entry, child) = if let Some(file) = include {
        let rel = Path::new(&file);
        if file.chars().any(char::is_control)
            || rel.is_absolute()
            || !rel.extension().is_some_and(|e| {
                ["yaml", "yml", "xml"]
                    .iter()
                    .any(|s| e.eq_ignore_ascii_case(s))
            })
        {
            return Err(
                "--include must be a relative .yaml, .yml or .xml path (relative to the changelog)"
                    .into(),
            );
        }
        let target = dir.join(rel);
        let child_text = if is_xml(&target) {
            format!("<databaseChangeLog>\n{xml_skeleton}\n</databaseChangeLog>\n")
        } else {
            format!(
                "databaseChangeLog:\n{}",
                serde_yaml::to_string(&vec![&skeleton]).map_err(|e| e.to_string())?
            )
        };
        (
            serde_yaml::to_value(serde_json::json!({"include": {"file": file}}))
                .map_err(|e| e.to_string())?,
            format!("  <include file=\"{}\"/>", escape_xml(&file)),
            Some((target, child_text)),
        )
    } else {
        (skeleton, xml_skeleton, None)
    };
    // XML attributes use the migration reader's scalar typing (for example an
    // author named "true"), so compare against that representation when editing XML.
    let entry = if xml {
        parse(
            &format!("<databaseChangeLog>{xml_entry}</databaseChangeLog>"),
            true,
        )?["databaseChangeLog"][0]
            .clone()
    } else {
        entry
    };
    let updated = append(&text, &doc, &entry, &xml_entry, xml)?;
    let temporary = path.with_extension(format!("tinox-new-{}.tmp", std::process::id()));
    write_new(&temporary, &updated)?;
    let result = (|| {
        if path.exists() {
            fs::set_permissions(
                &temporary,
                fs::metadata(&path)
                    .map_err(|e| e.to_string())?
                    .permissions(),
            )
            .map_err(|e| e.to_string())?;
        }
        if let Some((target, text)) = &child {
            fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
            write_new(target, text)?;
        }
        if let Err(e) = fs::rename(&temporary, &path) {
            if let Some((target, _)) = &child {
                let _ = fs::remove_file(target);
            }
            return Err(format!("cannot update {}: {e}", path.display()));
        }
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result?;
    println!(
        "Created changeSet {id} by {author} in {}",
        child.as_ref().map(|(p, _)| p).unwrap_or(&path).display()
    );
    println!("Add changes to the skeleton before running tinox db validate or update.");
    Ok(())
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).expect("string serialization")
}
