//! Boundary/memory edge cases.
//!
//! Strings at heap/alignment boundaries (Bug 15 was only visible at
//! length 15–16), special characters/UTF-8, empty/1-element/nested
//! containers. Ground truth is computed by the Rust host; the .tnx files
//! are generated at runtime. KNOWN_FAILURES like in matrix.rs.

mod common;
use common::{parse_case, run_case};
use std::fs;
use std::path::PathBuf;

const KNOWN_FAILURES: &[&str] = &[];

/// A generated case: name, Tinox source (without an expect header), expected lines.
fn cases() -> Vec<(String, String, Vec<String>)> {
    let mut out = Vec::new();

    // ── String lengths at heap boundaries ────────────────────────────────
    // Every length: literal len, len after a function return, a split
    // element (the Bug-15 path: elements of 15/16 characters
    // disappeared), and concatenation.
    let lengths = [0usize, 1, 7, 8, 15, 16, 17, 31, 32];
    let mut body = String::new();
    let mut expects: Vec<String> = Vec::new();
    // Issue #149 stage 3: no top-level `fn` allowed anymore -- `ident`
    // becomes a sibling `fnc` in the same `class Main` as `main`, called
    // bare exactly as before (same-class fallback).
    body.push_str("class Main {\n    fnc ident(s: String) -> String {\n        return s;\n    }\n\n    fnc main() -> Int32 {\n");
    for (i, l) in lengths.iter().enumerate() {
        let s: String = "abcdefghijklmnopqrstuvwxyzABCDEF"[..*l].to_string();
        body.push_str(&format!("    let s{i}: String = \"{s}\";\n"));
        body.push_str(&format!("    println(s{i}.len());\n"));
        expects.push(l.to_string());
        body.push_str(&format!("    println(ident(s{i}).len());\n"));
        expects.push(l.to_string());
        // split: an element of length L between two anchors
        body.push_str(&format!(
            "    let parts{i} = (\"x|\" + s{i} + \"|y\").split(\"|\");\n"
        ));
        body.push_str(&format!("    println(parts{i}.len());\n"));
        expects.push("3".to_string());
        body.push_str(&format!("    println(parts{i}[1].len());\n"));
        expects.push(l.to_string());
        // Concatenation correctly changes the length
        body.push_str(&format!("    println((s{i} + \"!\").len());\n"));
        expects.push((l + 1).to_string());
    }
    body.push_str("    return 0;\n    }\n}\n");
    out.push(("boundary_string_lengths".to_string(), body, expects));

    // ── Special characters ────────────────────────────────────────────────
    let src = r##"class Main {
    fnc main() -> Int32 {
    let hash = "# kein Raw-String";
    println(hash.len());
    println(hash);
    let quoted = "sag \"hi\"";
    println(quoted.len());
    println(quoted);
    let nl = "a\nb";
    println(nl.len());
    println(nl.split("\n").len());
    let spaces = "  mitte  ";
    println(spaces.len());
    println(spaces + "|");
    let uml = "äöü";
    println(uml.len());
    println(uml);
    let tab = "a\tb";
    println(tab.len());
    return 0;
    }
}
"##;
    let expects = vec![
        "17".into(), "# kein Raw-String".into(),
        "8".into(), "sag \"hi\"".into(),
        "3".into(), "2".into(),
        "9".into(), "  mitte  |".into(),
        "6".into(), "äöü".into(),   // len counts bytes (UTF-8: 2 per umlaut)
        "3".into(),
    ];
    out.push(("boundary_string_special".to_string(), src.to_string(), expects));

    // ── Empty and 1-element containers ─────────────────────────────
    let src = r#"class Main {
    fnc main() -> Int32 {
    var e: List<Int64> = [];
    println(e.len());
    var n = 0;
    for x in e { n += 1; }
    println(n);
    e.push(42);
    println(e.len());
    println(e[0]);
    println(e.first());
    println(e.last());
    e.pop();
    println(e.len());
    let one: List<String> = ["solo"];
    println(one.len());
    println(one[0]);
    println(one.first());
    println(one.last());
    var m: Map<String, Int64> = Map::new();
    println(m.len());
    println(m.keys().len());
    if (m.contains("x")) { println("y"); } else { println("n"); }
    return 0;
    }
}
"#;
    let expects = vec![
        "0".into(), "0".into(), "1".into(), "42".into(), "42".into(), "42".into(), "0".into(),
        "1".into(), "solo".into(), "solo".into(), "solo".into(),
        "0".into(), "0".into(), "n".into(),
    ];
    out.push(("boundary_container_empty_one".to_string(), src.to_string(), expects));

    // ── Nesting: 3 levels of lists ─────────────────────────────
    let src = r#"class Main {
    fnc main() -> Int32 {
    let deep: List<List<List<Int64>>> = [[[1, 2], [3]], [[4]]];
    println(deep.len());
    println(deep[0].len());
    println(deep[0][0].len());
    println(deep[0][0][1]);
    println(deep[0][1][0]);
    println(deep[1][0][0]);
    var total = 0;
    for lvl2 in deep {
        for lvl3 in lvl2 {
            for x in lvl3 { total += x; }
        }
    }
    println(total);
    return 0;
    }
}
"#;
    let expects = vec![
        "2".into(), "2".into(), "2".into(), "2".into(), "3".into(), "4".into(), "10".into(),
    ];
    out.push(("boundary_list_nested3".to_string(), src.to_string(), expects));

    // ── A list of maps ───────────────────────────────────────────────
    let src = r#"class Main {
    fnc main() -> Int32 {
    var m1: Map<String, Int64> = Map::new();
    m1.insert("a", 1);
    var m2: Map<String, Int64> = Map::new();
    m2.insert("b", 2);
    m2.insert("c", 3);
    let ms: List<Map<String, Int64>> = [m1, m2];
    println(ms.len());
    println(ms[0].len());
    println(ms[1].len());
    println(ms[0].get("a"));
    println(ms[1].get("c"));
    return 0;
    }
}
"#;
    let expects = vec!["2".into(), "1".into(), "2".into(), "1".into(), "3".into()];
    out.push(("boundary_list_of_maps".to_string(), src.to_string(), expects));

    // ── A map with list values ────────────────────────────────────────
    let src = r#"class Main {
    fnc main() -> Int32 {
    var m: Map<String, List<Int64>> = Map::new();
    m.insert("xs", [10, 20]);
    let xs: List<Int64> = m.get("xs");
    println(xs.len());
    println(xs[1]);
    var s = 0;
    for x in xs { s += x; }
    println(s);
    return 0;
    }
}
"#;
    let expects = vec!["2".into(), "20".into(), "30".into()];
    out.push(("boundary_map_list_values".to_string(), src.to_string(), expects));

    out
}

fn generate_all(shard: usize) -> PathBuf {
    // A separate directory per shard — shards run as parallel threads, a
    // shared directory would cause remove/rewrite races.
    let dir = std::env::temp_dir().join(format!("tinox-boundary-{}-{shard}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir boundary dir");
    for (name, body, expects) in cases() {
        let mut src = String::new();
        for e in &expects {
            src.push_str(&format!("// expect: {e}\n"));
        }
        src.push('\n');
        src.push_str(&body);
        // Issue #149 stage 3: `class Main` (see cases() above) requires the
        // file to be named exactly `Main.tnx` -- each case gets its own
        // subdirectory, same convention e2e.rs uses for directory cases.
        let case_dir = dir.join(&name);
        fs::create_dir_all(&case_dir).expect("mkdir case dir");
        fs::write(case_dir.join("Main.tnx"), src).expect("write case");
    }
    dir
}

fn run_shard(shard: usize, num_shards: usize) {
    let dir = generate_all(shard);
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    names.sort();

    let mut unexpected = Vec::new();
    let mut stale = Vec::new();
    for (i, name) in names.iter().enumerate() {
        if i % num_shards != shard {
            continue;
        }
        // `parse_case` sets `case.name` from the file stem ("Main") --
        // override back to the unique per-case name, same reason as
        // stdlib_smoke.rs (workdir/output-binary collision across
        // concurrently-run cases otherwise).
        let mut case = parse_case(&dir.join(name).join("Main.tnx"));
        case.name = name.clone();
        let known_bad = KNOWN_FAILURES.contains(&name.as_str());
        match run_case(&case) {
            Ok(()) if known_bad => stale.push(name.clone()),
            Ok(()) => {}
            Err(_) if known_bad => {}
            Err(msg) => unexpected.push(format!("== {name} ==\n{msg}")),
        }
    }

    let mut problems = Vec::new();
    if !unexpected.is_empty() {
        problems.push(format!(
            "{} boundary cases fail:\n\n{}",
            unexpected.len(),
            unexpected.join("\n\n")
        ));
    }
    if !stale.is_empty() {
        problems.push(format!(
            "stale KNOWN_FAILURES (now passing — remove the entry): {}",
            stale.join(", ")
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n\n"));
}

#[test]
fn boundary_shard_0() { run_shard(0, 3); }
#[test]
fn boundary_shard_1() { run_shard(1, 3); }
#[test]
fn boundary_shard_2() { run_shard(2, 3); }
