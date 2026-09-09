//! Property tests at the runtime level (TESTPLAN Phase 2.1).
//!
//! Random inputs (deterministically seeded, no dependency), from which
//! .tnx programs are generated whose expected output is computed by the
//! Rust host. One file per property with multiple instances — compile
//! time dominates, so many checks per program instead of many programs.
//!
//! To reproduce: the seed is in the failure report; `TINOX_PROP_SEED=<n>` sets it.

mod common;
use common::{parse_case, run_case};
use std::fs;
use std::path::PathBuf;

const INSTANCES_PER_PROPERTY: usize = 12;

/// xorshift64* — plenty for a deterministic source.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    /// ASCII without escaping needs (no quotes/backslashes)
    fn ident_string(&mut self, max_len: u64) -> String {
        const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 _.,!?#=+-*";
        let len = self.below(max_len + 1);
        (0..len)
            .map(|_| ALPHA[self.below(ALPHA.len() as u64) as usize] as char)
            .collect()
    }
    fn int_list(&mut self, max_len: u64) -> Vec<i64> {
        let len = self.below(max_len + 1);
        (0..len).map(|_| self.next() as i64 % 1000).collect()
    }
}

fn seed() -> u64 {
    std::env::var("TINOX_PROP_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x7451_0709_2026_0001)
}

/// A property: name + generator that yields (statements, expected lines)
/// per instance. Statements may create v{i}-prefixed variables.
type Instance = (String, Vec<String>);

fn prop_join_split(rng: &mut Rng, i: usize) -> Instance {
    // split(join(xs, "|"), "|") == xs — elements without '|'
    let n = rng.below(5) + 1;
    let xs: Vec<String> = (0..n).map(|_| rng.ident_string(9)).collect();
    let lit = xs
        .iter()
        .map(|s| format!("\"{s}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmts = format!("    let xs{i}: List<String> = [{lit}];\n");
    stmts.push_str(&format!("    let joined{i} = xs{i}.join(\"|\");\n"));
    stmts.push_str(&format!("    let back{i} = joined{i}.split(\"|\");\n"));
    stmts.push_str(&format!("    println(back{i}.len());\n"));
    stmts.push_str(&format!("    for x in back{i} {{ println(x + \"$\"); }}\n"));
    let mut expects = vec![xs.len().to_string()];
    expects.extend(xs.iter().map(|s| format!("{s}$")));
    (stmts, expects)
}

fn prop_substring(rng: &mut Rng, i: usize) -> Instance {
    // s.substring(a, b) == host slice; substring(0, len) == s
    let s = rng.ident_string(24);
    let len = s.len() as u64;
    let a = rng.below(len + 1);
    let b = a + rng.below(len - a + 1);
    let expected = &s[a as usize..b as usize];
    let stmts = format!(
        "    let s{i} = \"{s}\";\n    println(s{i}.substring({a}, {b}) + \"$\");\n    println(s{i}.substring(0, s{i}.len()) + \"$\");\n"
    );
    (stmts, vec![format!("{expected}$"), format!("{s}$")])
}

fn prop_concat(rng: &mut Rng, i: usize) -> Instance {
    // (a + b).len() == a.len() + b.len(); content matches
    let a = rng.ident_string(12);
    let b = rng.ident_string(12);
    let stmts = format!(
        "    let a{i} = \"{a}\";\n    let b{i} = \"{b}\";\n    let c{i} = a{i} + b{i};\n    println(c{i}.len());\n    println(c{i} + \"$\");\n"
    );
    (stmts, vec![(a.len() + b.len()).to_string(), format!("{a}{b}$")])
}

fn prop_sort(rng: &mut Rng, i: usize) -> Instance {
    // sort() == host-sorted; original unchanged (sort returns fresh)
    let xs = rng.int_list(8);
    let lit = xs.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");
    let mut sorted = xs.clone();
    sorted.sort();
    let mut stmts = format!("    let xs{i}: List<Int64> = [{lit}];\n");
    stmts.push_str(&format!("    let s{i} = xs{i}.sort();\n"));
    stmts.push_str(&format!("    for x in s{i} {{ println(x); }}\n"));
    stmts.push_str(&format!("    println(xs{i}.len());\n"));
    let mut expects: Vec<String> = sorted.iter().map(|v| v.to_string()).collect();
    expects.push(xs.len().to_string());
    (stmts, expects)
}

fn prop_reverse(rng: &mut Rng, i: usize) -> Instance {
    // reverse(reverse(xs)) == xs and reverse == host reverse
    let xs = rng.int_list(8);
    let lit = xs.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");
    let mut stmts = format!("    let xs{i}: List<Int64> = [{lit}];\n");
    stmts.push_str(&format!("    let r{i} = xs{i}.reverse();\n"));
    stmts.push_str(&format!("    for x in r{i} {{ println(x); }}\n"));
    stmts.push_str(&format!("    let rr{i} = r{i}.reverse();\n"));
    stmts.push_str(&format!("    for x in rr{i} {{ println(x); }}\n"));
    let mut expects: Vec<String> = xs.iter().rev().map(|v| v.to_string()).collect();
    expects.extend(xs.iter().map(|v| v.to_string()));
    (stmts, expects)
}

fn prop_push_pop(rng: &mut Rng, i: usize) -> Instance {
    // Model comparison: sequence of push/pop against Vec<i64>
    let mut model: Vec<i64> = Vec::new();
    let mut stmts = format!("    var xs{i}: List<Int64> = [];\n");
    for _ in 0..rng.below(12) + 3 {
        if rng.below(3) == 0 && !model.is_empty() {
            model.pop();
            stmts.push_str(&format!("    xs{i}.pop();\n"));
        } else {
            let v = rng.next() as i64 % 1000;
            model.push(v);
            stmts.push_str(&format!("    xs{i}.push({v});\n"));
        }
    }
    stmts.push_str(&format!("    println(xs{i}.len());\n"));
    stmts.push_str(&format!("    for x in xs{i} {{ println(x); }}\n"));
    let mut expects = vec![model.len().to_string()];
    expects.extend(model.iter().map(|v| v.to_string()));
    (stmts, expects)
}

fn prop_contains_index_of(rng: &mut Rng, i: usize) -> Instance {
    // contains/indexOf consistent with the host for hits and misses
    let mut xs = rng.int_list(7);
    xs.push(rng.next() as i64 % 1000); // nie leer
    let lit = xs.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");
    let hit = xs[rng.below(xs.len() as u64) as usize];
    let miss = 5000 + (rng.next() as i64 % 1000).abs(); // outside the value range
    let hit_idx = xs.iter().position(|&v| v == hit).unwrap();
    let stmts = format!(
        "    let xs{i}: List<Int64> = [{lit}];\n\
         \x20   if (xs{i}.contains({hit})) {{ println(\"y\"); }} else {{ println(\"n\"); }}\n\
         \x20   if (xs{i}.contains({miss})) {{ println(\"y\"); }} else {{ println(\"n\"); }}\n\
         \x20   println(xs{i}.indexOf({hit}));\n\
         \x20   println(xs{i}.indexOf({miss}));\n"
    );
    (
        stmts,
        vec!["y".into(), "n".into(), hit_idx.to_string(), "-1".into()],
    )
}

fn prop_replace(rng: &mut Rng, i: usize) -> Instance {
    // s.replace(from, to) == host replace (all occurrences)
    let base = rng.ident_string(10);
    let from = rng.ident_string(3);
    let to = rng.ident_string(4);
    // from must not be empty (Tinox: empty from → unchanged, avoid the host special case)
    let from = if from.is_empty() { "q".to_string() } else { from };
    let s = format!("{base}{from}{base}");
    let expected = s.replace(&from, &to);
    let stmts = format!(
        "    let s{i} = \"{s}\";\n    println(s{i}.replace(\"{from}\", \"{to}\") + \"$\");\n"
    );
    (stmts, vec![format!("{expected}$")])
}

type PropFn = fn(&mut Rng, usize) -> Instance;

const PROPERTIES: &[(&str, PropFn)] = &[
    ("join_split_roundtrip", prop_join_split),
    ("substring_host_oracle", prop_substring),
    ("concat_len_and_content", prop_concat),
    ("sort_matches_host", prop_sort),
    ("reverse_involution", prop_reverse),
    ("push_pop_model", prop_push_pop),
    ("contains_indexof_consistent", prop_contains_index_of),
    ("replace_host_oracle", prop_replace),
];

fn generate(shard: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tinox-props-{}-{shard}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir props dir");
    for (name, gen) in PROPERTIES {
        // Separate, name-dependent seed stream per property
        let mut rng = Rng(seed() ^ name.bytes().map(u64::from).sum::<u64>().wrapping_mul(0x9E3779B9));
        // Issue #149 stage 3: no top-level `fn` allowed anymore -- wrap in
        // the fixed `class Main { fnc main() -> Int32 }` entry-point shape.
        let mut body = String::from("class Main {\n    fnc main() -> Int32 {\n");
        let mut expects = Vec::new();
        for i in 0..INSTANCES_PER_PROPERTY {
            let (stmts, exp) = gen(&mut rng, i);
            body.push_str(&stmts);
            expects.extend(exp);
        }
        body.push_str("    return 0;\n    }\n}\n");
        let mut src = String::new();
        for e in &expects {
            src.push_str(&format!("// expect: {e}\n"));
        }
        src.push('\n');
        src.push_str(&body);
        // `class Main` requires the file to be named exactly `Main.tnx`
        // (one-class-per-file) -- each property gets its own subdirectory,
        // same convention e2e.rs uses for directory cases.
        let case_dir = dir.join(format!("prop_{name}"));
        fs::create_dir_all(&case_dir).expect("mkdir case dir");
        fs::write(case_dir.join("Main.tnx"), src).expect("write case");
    }
    dir
}

fn run_shard(shard: usize, num_shards: usize) {
    let dir = generate(shard);
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    names.sort();

    let mut failures = Vec::new();
    for (i, name) in names.iter().enumerate() {
        if i % num_shards != shard {
            continue;
        }
        // `parse_case` sets `case.name` from the file stem ("Main") --
        // override back to the unique per-case name, same reason as
        // stdlib_smoke.rs/boundary.rs (workdir/output-binary collision
        // across concurrently-run cases otherwise).
        let mut case = parse_case(&dir.join(name).join("Main.tnx"));
        case.name = name.clone();
        if let Err(msg) = run_case(&case) {
            failures.push(format!("== {name} (seed {}) ==\n{msg}", seed()));
        }
    }
    assert!(
        failures.is_empty(),
        "{} property cases violated (reproduce: TINOX_PROP_SEED={}):\n\n{}",
        failures.len(),
        seed(),
        failures.join("\n\n")
    );
}

#[test]
fn properties_shard_0() { run_shard(0, 4); }
#[test]
fn properties_shard_1() { run_shard(1, 4); }
#[test]
fn properties_shard_2() { run_shard(2, 4); }
#[test]
fn properties_shard_3() { run_shard(3, 4); }
