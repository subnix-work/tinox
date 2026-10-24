//! Compare catalog snapshots from the live database and an isolated changelog replay.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

type Catalog = BTreeMap<(String, String, String), BTreeSet<String>>;

fn snapshot(exe: &Path, mode: &str) -> Result<Catalog, String> {
    let out = Command::new(exe)
        .env("TINOX_MIGRATE_MODE", mode)
        .output()
        .map_err(|e| format!("cannot run schema inspection: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "schema inspection failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let mut catalog = Catalog::new();
    let decode = |s: &str| -> Result<String, String> {
        if !s.len().is_multiple_of(2) {
            return Err("invalid catalog encoding".into());
        }
        let bytes = s
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                let digit = |b: u8| (b as char).to_digit(16).ok_or("invalid catalog encoding");
                Ok((digit(c[0])? * 16 + digit(c[1])?) as u8)
            })
            .collect::<Result<Vec<_>, String>>()?;
        String::from_utf8(bytes).map_err(|e| format!("invalid catalog text: {e}"))
    };
    for line in String::from_utf8(out.stdout)
        .map_err(|e| e.to_string())?
        .lines()
    {
        let fields = line
            .split('\t')
            .map(decode)
            .collect::<Result<Vec<_>, _>>()?;
        let [kind, table, name, definition]: [String; 4] = fields
            .try_into()
            .map_err(|_| "invalid catalog row".to_string())?;
        catalog
            .entry((kind, table, name))
            .or_default()
            .insert(definition);
    }
    Ok(catalog)
}

pub fn run(exe: &Path) -> Result<bool, String> {
    // Inspect live first, but replay only in the separate shadow process/connection.
    let live = snapshot(exe, "diff-live")?;
    let expected = snapshot(exe, "diff-shadow")?;
    let keys: BTreeSet<_> = live.keys().chain(expected.keys()).collect();
    let mut drift = 0;
    for key @ (kind, table, name) in keys {
        let object = if name.is_empty() {
            table.clone()
        } else {
            format!("{table}.{name}")
        };
        match (expected.get(key), live.get(key)) {
            (Some(want), Some(got)) if want != got => {
                println!("changed {kind} {object}: expected {want:?}, live {got:?}");
            }
            (Some(_), None) => println!("missing {kind} {object}"),
            (None, Some(_)) => println!("unexpected {kind} {object}"),
            _ => continue,
        }
        drift += 1;
    }
    if drift == 0 {
        println!("No schema drift detected.");
    } else {
        println!("Schema drift detected: {drift} difference(s).");
    }
    Ok(drift != 0)
}
