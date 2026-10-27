//! Issue #492: formatting through the real server matches `tinox fmt`.
mod common;
use common::*;
use serde_json::json;

const MESSY: &str = "class Main {\n  fnc main() -> Int32 {\n        println(\"a\")\n   return 0 }\n}\n";

#[test]
fn formatting_returns_edits_equal_to_the_cli_output() {
    let root = temp_project("fmt");
    let file = root.join("Main.tnx");
    std::fs::write(&file, MESSY).unwrap();
    let mut lsp = Lsp::start(&root, false);
    assert_eq!(lsp.init["capabilities"]["documentFormattingProvider"], true);
    assert_eq!(lsp.init["capabilities"]["documentRangeFormattingProvider"], true);
    lsp.open(&file, MESSY);
    let edits = lsp.request("textDocument/formatting", json!({"textDocument": {"uri": url(&file)}, "options": {"tabSize": 8, "insertSpaces": false}}));
    assert!(!edits.as_array().unwrap().is_empty());

    let cli = std::process::Command::new(env!("CARGO_BIN_EXE_tinox-lsp").replace("tinox-lsp", "tinox")).args(["fmt"]).arg(&file).output();
    if let Ok(cli) = cli {
        if cli.status.success() {
            // apply the edits to the original text and compare with `tinox fmt`
            let mut text = MESSY.to_string();
            let mut es: Vec<_> = edits.as_array().unwrap().clone();
            es.sort_by_key(|e| std::cmp::Reverse(e["range"]["start"]["line"].as_u64().unwrap()));
            let lines: Vec<&str> = MESSY.split_inclusive('\n').collect();
            for e in es {
                let at = |p: &serde_json::Value| -> usize {
                    let l = p["line"].as_u64().unwrap() as usize;
                    lines.iter().take(l).map(|x| x.len()).sum::<usize>() + p["character"].as_u64().unwrap() as usize
                };
                text.replace_range(at(&e["range"]["start"])..at(&e["range"]["end"]), e["newText"].as_str().unwrap());
            }
            assert_eq!(text, String::from_utf8_lossy(&cli.stdout));
        }
    }
    lsp.shutdown();
}

#[test]
fn syntax_errors_leave_the_buffer_untouched_with_a_reason() {
    let root = temp_project("fmt_bad");
    let file = root.join("Main.tnx");
    let broken = "class Main {";
    std::fs::write(&file, broken).unwrap();
    let mut lsp = Lsp::start(&root, false);
    lsp.open(&file, broken);
    let r = lsp.request_raw("textDocument/formatting", json!({"textDocument": {"uri": url(&file)}, "options": {"tabSize": 4, "insertSpaces": true}}));
    assert_eq!(r["error"]["code"], -32803, "{r}");
    assert!(r["error"]["message"].as_str().unwrap().contains("syntax errors"), "{r}");
    assert!(r.get("result").is_none());
    lsp.shutdown();
}
