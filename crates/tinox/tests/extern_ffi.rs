//! Issue #486: `extern fn` C ABI contract, exercised against real C code
//! compiled through the `[native]` section (#485).
use std::path::PathBuf;
use std::process::Command;

fn tinox() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tinox"))
}

const C_SRC: &str = r#"
#include <stdint.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <gc.h>

static int64_t counter = 0;

bool    ffi_not(bool b)               { return !b; }
int8_t  ffi_add8(int8_t a, int8_t b)  { return (int8_t)(a + b); }
uint8_t ffi_addu8(uint8_t a, uint8_t b) { return (uint8_t)(a + b); }
int16_t ffi_neg16(int16_t a)          { return (int16_t)-a; }
int32_t ffi_mul32(int32_t a, int32_t b) { return a * b; }
double  ffi_half(double x)            { return x / 2.0; }
int64_t ffi_sum6(int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f) {
    return a + b + c + d + e + f;
}
void    ffi_bump(int64_t by)          { counter += by; }
int64_t ffi_counter(void)             { return counter; }
int64_t ffi_strlen(const char* s)     { return (int64_t)strlen(s); }
const char* ffi_static(void)          { return "static-string"; }
char* ffi_greet(const char* name) {
    size_t n = strlen(name) + 8;
    char* out = GC_malloc_atomic(n);
    snprintf(out, n, "hello, %s", name);
    return out;
}
"#;

const MAIN: &str = r#"
extern fn ffi_not(b: Bool) -> Bool
extern fn ffi_add8(a: Int8, b: Int8) -> Int8
extern fn ffi_addu8(a: UInt8, b: UInt8) -> UInt8
extern fn ffi_neg16(a: Int16) -> Int16
extern fn ffi_mul32(a: Int32, b: Int32) -> Int32
extern fn ffi_half(x: Float64) -> Float64
extern fn ffi_sum6(a: Int64, b: Int64, c: Int64, d: Int64, e: Int64, f: Int64) -> Int64
extern fn ffi_bump(by: Int64)
extern fn ffi_counter() -> Int64
extern fn ffi_strlen(s: String) -> Int64
extern fn ffi_static() -> String
extern fn ffi_greet(name: String) -> String

class Main
{
    fnc main() -> Int32 {
        println("not=" + ffi_not(true).toString() + "," + ffi_not(false).toString())
        println("add8=" + ffi_add8(100, 100).toString())
        println("addu8=" + ffi_addu8(200, 100).toString())
        println("neg16=" + ffi_neg16(300).toString())
        println("mul32=" + ffi_mul32(70000, 3).toString())
        println("half=" + ffi_half(5.0).toString())
        println("sum6=" + ffi_sum6(1, 2, 3, 4, 5, 6).toString())
        ffi_bump(40)
        ffi_bump(2)
        println("counter=" + ffi_counter().toString())
        println("strlen=" + ffi_strlen("héllo").toString())
        println("static=" + ffi_static())
        println("greet=" + ffi_greet("tinox") + "!")
        // GC stress: strings returned from C must survive collections.
        var kept: List<String> = []
        var i: Int64 = 0
        while i < 20000 {
            kept.push(ffi_greet("n" + i.toString()))
            i = i + 1
        }
        println("kept=" + kept.len().toString() + " last=" + kept[19999])
        return 0
    }
}
"#;

fn project(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("tinox_ffi_{}_{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("tinox.toml"),
        format!(
            "[package]\nname = \"ffi_demo\"\nversion = \"0.1.0\"\ncompiler = \"{}\"\n\n[native]\nsources = [\"ffi.c\"]\n",
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    std::fs::write(d.join("ffi.c"), C_SRC).unwrap();
    std::fs::write(d.join("Main.tnx"), MAIN).unwrap();
    d
}

#[test]
fn extern_fn_follows_the_c_abi_for_every_supported_scalar_and_string() {
    let d = project("abi");
    let out = d.join("app");
    let r = tinox().current_dir(&d).args(["build", "Main.tnx", "-o", out.to_str().unwrap()]).output().unwrap();
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    let run = Command::new(&out).output().unwrap();
    let stdout = String::from_utf8_lossy(&run.stdout);
    let expected = [
        "not=false,true",
        "add8=-56",
        "addu8=44",
        "neg16=-300",
        "mul32=210000",
        "half=2.5",
        "sum6=21",
        "counter=42",
        "strlen=6",
        "static=static-string",
        "greet=hello, tinox!",
        "kept=20000 last=hello, n19999",
    ];
    assert_eq!(stdout.lines().collect::<Vec<_>>(), expected, "stderr: {}", String::from_utf8_lossy(&run.stderr));
}

fn check_fails(decl: &str, needle: &str) {
    let d = std::env::temp_dir().join(format!("tinox_ffi_bad_{}_{}", needle.len(), std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let src = d.join("Main.tnx");
    std::fs::write(&src, format!("{decl}\n\nclass Main\n{{\n    fnc main() -> Int32 {{ return 0 }}\n}}\n")).unwrap();
    let r = tinox().args(["check", src.to_str().unwrap()]).output().unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&r.stdout), String::from_utf8_lossy(&r.stderr));
    assert!(!r.status.success(), "expected failure for `{decl}`");
    assert!(text.contains(needle), "expected `{needle}` in:\n{text}");
}

#[test]
fn extern_fn_rejects_types_without_a_c_representation() {
    check_fails("extern fn f(cb: fnc(Int64) -> Int64) -> Int64", "function types");
    check_fails("extern fn f(t: (Int64, Int64)) -> Int64", "tuples");
    check_fails("extern fn f(x: Int64?) -> Int64", "nullable types");
    check_fails("extern fn f() -> Int64?", "return type uses nullable types");
    check_fails("extern fn f(x: Nothing) -> Int64", "cannot be `Nothing`");
}

fn build_source(name: &str, source: &str) -> std::process::Output {
    let d = std::env::temp_dir().join(format!("tinox_extern_redecl_{}_{}", name, std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("Main.tnx"), source).unwrap();
    tinox().current_dir(&d).args(["build", "Main.tnx", "-o", "app"]).output().unwrap()
}

// Issue #504: the compiler declares the runtime symbols it uses itself; an
// `extern fn` naming one of them used to reach LLVM as a second `declare` and
// fail as an internal compiler error.
#[test]
fn extern_fn_may_redeclare_a_runtime_symbol_with_the_same_signature() {
    let src = "extern fn tinox_clock_nanos() -> Int64\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        const a: Int64 = tinox_clock_nanos()\n        const b: Int64 = tinox_clock_nanos()\n        println((b >= a).toString())\n        return 0\n    }\n}\n";
    let out = build_source("same", src);
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let run = Command::new(std::env::temp_dir().join(format!("tinox_extern_redecl_same_{}", std::process::id())).join("app")).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "true");
}

#[test]
fn extern_fn_with_a_different_signature_than_a_runtime_symbol_is_a_clear_error() {
    let src = "extern fn tinox_clock_nanos() -> Int32\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        return tinox_clock_nanos()\n    }\n}\n";
    let out = build_source("different", src);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success());
    assert!(text.contains("extern fn `tinox_clock_nanos` conflicts with the declaration the compiler already emits"), "{text}");
    assert!(text.contains("declare i64 @tinox_clock_nanos()") && text.contains("declare i32 @tinox_clock_nanos()"), "both signatures are named: {text}");
    assert!(!text.contains("internal compiler error"), "{text}");
}
