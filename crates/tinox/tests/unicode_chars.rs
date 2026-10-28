//! Unicode scalar iteration (#566), checked against Rust's independent decoder.
use std::process::Command;

#[test]
fn unicode_chars_validates_native_strings_and_preserves_scalars() {
    let dir = std::env::temp_dir().join(format!("tinox_unicode_chars_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tinox.toml"), format!(
        "[package]\nname = \"unicode_chars\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\n\n[native]\nsources = [\"input.c\"]\n", env!("CARGO_PKG_VERSION")
    )).unwrap();
    std::fs::write(dir.join("Main.tnx"), r#"
extern fn native_text() -> String
extern fn native_calls() -> Int64
class Main {
    fnc main() -> Int32 {
        const s = native_text()
        const chars: List<String> = s.chars()
        println(chars.len())
        for ch in chars {
            println(ch.len())
            println(ch)
        }
        println(chars.join("") == s)
        const direct = String_chars(native_text())
        const staticChars = String::chars(native_text())
        const instanceChars = native_text().chars()
        println(direct.join("") == s)
        println(staticChars.join("") == s)
        println(instanceChars.join("") == s)
        println(native_calls())
        return 0
    }
}
"#).unwrap();
    // Includes all UTF-8 widths/boundaries, a combining mark, and a multi-scalar
    // emoji sequence. Grapheme clusters deliberately remain separate scalars.
    let valid = "A\u{7f}\u{80}\u{7ff}\u{800}\u{d7ff}\u{e000}\u{ffff}\u{10000}\u{10ffff}é…e\u{301}👩\u{200d}💻";
    let cases: Vec<Vec<u8>> = vec![
        valid.as_bytes().to_vec(), vec![], b"a\0b".to_vec(),
        vec![0x80], vec![0xc0, 0x80], vec![0xc1, 0xbf], vec![0xc2],
        vec![0xe0, 0x80, 0x80], vec![0xed, 0xa0, 0x80], vec![0xed, 0xbf, 0xbf],
        vec![0xf0, 0x80, 0x80, 0x80], vec![0xf4, 0x90, 0x80, 0x80],
        vec![0xf5, 0x80, 0x80, 0x80], vec![0xf0, 0x9f, 0x98],
        vec![b'A', 0xe2, 0x28, 0xa1], vec![0xff],
    ];
    let mut c = String::from("#include <stdlib.h>\n#include <stdint.h>\n");
    for (i, bytes) in cases.iter().enumerate() {
        let contents = bytes.iter().chain(std::iter::once(&0)).map(u8::to_string).collect::<Vec<_>>().join(",");
        c.push_str(&format!("static unsigned char input_{i}[] = {{{contents}}};\n"));
    }
    c.push_str("static int64_t calls;\nint64_t native_calls(void) { return calls; }\nchar* native_text(void) { calls++; switch (atoi(getenv(\"TINOX_UTF8_CASE\"))) {\n");
    for i in 0..cases.len() { c.push_str(&format!("case {i}: return (char*)input_{i};\n")); }
    c.push_str("default: return 0; } }\n");
    std::fs::write(dir.join("input.c"), c).unwrap();
    let binary = dir.join("app");
    let build = Command::new(env!("CARGO_BIN_EXE_tinox")).current_dir(&dir)
        .args(["build", "Main.tnx", "-o"]).arg(&binary).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    for (i, bytes) in cases.iter().enumerate() {
        let run = Command::new(&binary).env("TINOX_UTF8_CASE", i.to_string()).output().unwrap();
        // Existing String ABI ends at the first NUL, including native inputs.
        let visible = bytes.split(|b| *b == 0).next().unwrap();
        match std::str::from_utf8(visible) {
            Ok(text) => {
                assert!(run.status.success(), "case {i}: {}", String::from_utf8_lossy(&run.stderr));
                let mut expected = format!("{}\n", text.chars().count());
                for ch in text.chars() { expected.push_str(&format!("{}\n{}\n", ch.len_utf8(), ch)); }
                expected.push_str("true\ntrue\ntrue\ntrue\n4\n");
                assert_eq!(String::from_utf8(run.stdout).unwrap(), expected, "case {i}");
            }
            Err(err) => {
                assert_eq!(run.status.code(), Some(1), "case {i}");
                assert!(run.stdout.is_empty(), "must validate before returning any elements");
                assert_eq!(String::from_utf8_lossy(&run.stderr), format!(
                    "runtime error: String.chars invalid UTF-8 at byte {}\n", err.valid_up_to()
                ), "case {i}");
            }
        }
    }
    let null = Command::new(&binary).env("TINOX_UTF8_CASE", "99").output().unwrap();
    assert_eq!(null.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&null.stderr).contains("requires a non-null string"));
    std::fs::remove_dir_all(dir).unwrap();
}
