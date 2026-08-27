use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use tinox_codegen::CodeGen;
use tinox_lexer::Lexer;
use tinox_parser::Parser;
use tinox_typecheck::TypeChecker;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn runtime_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../runtime/runtime.c")
        .canonicalize()
        .expect("runtime.c not found")
}

/// Compile a Tinox source string, run it, and return trimmed stdout.
/// Panics with a descriptive message on any failure.
fn run(source: &str) -> String {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = format!("/tmp/tinox_test_{}", id);

    // Pipeline
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap_or_else(|e| panic!("lex error: {:?}", e));

    let mut parser = Parser::new(tokens);
    let ast = parser.parse().unwrap_or_else(|e| panic!("parse error: {:?}", e));

    let mut tc = TypeChecker::new();
    let checked = tc.check(&ast).unwrap_or_else(|e| panic!("typecheck error: {}", e));
    let (iface_methods, class_implements) = tc.interface_info();

    let mut cg = CodeGen::new();
    cg.set_interface_info(iface_methods, class_implements);
    cg.gen(&checked).unwrap_or_else(|e| panic!("codegen error: {:?}", e));
    let ir = cg.into_ir();

    let ll = format!("{}.ll", tmp);
    let obj = format!("{}.o", tmp);
    let rt_obj = format!("{}_rt.o", tmp);

    fs::write(&ll, &ir).expect("write IR");

    assert!(
        Command::new("llc").args(["-O1", "-march=x86-64", "-filetype=obj", "-o", &obj, &ll])
            .status().expect("llc").success(),
        "llc failed"
    );
    assert!(
        Command::new("cc").args(["-c", runtime_src().to_str().unwrap(), "-o", &rt_obj])
            .status().expect("cc runtime").success(),
        "runtime compile failed"
    );
    assert!(
        Command::new("cc").args([&obj, &rt_obj, "-o", &tmp, "-lm", "-lpthread", "-lgc", "-lz", "-no-pie"])
            .status().expect("cc link").success(),
        "link failed"
    );

    let output = Command::new(&tmp).output().expect("run binary");

    for f in &[&ll, &obj, &rt_obj, &tmp] {
        let _ = fs::remove_file(f);
    }

    String::from_utf8(output.stdout).expect("utf8").trim().to_string()
}

// ── Hello World ──────────────────────────────────────────────────────────────

#[test]
fn test_hello_world() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    println("Hello, World!");
    return 0;
}
"#), "Hello, World!");
}

// ── Variables & Arithmetic ────────────────────────────────────────────────────

#[test]
fn test_variables_int() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let x: Int64 = 42;
    let y = 8;
    println(x + y);
    return 0;
}
"#), "50");
}

#[test]
fn test_variables_float() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let pi: Float64 = 3.14;
    let r: Float64 = 2.0;
    let area = pi * r * r;
    println(area);
    return 0;
}
"#), "12.56");
}

#[test]
fn test_variables_bool() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let t = true;
    let f = false;
    println(t);
    println(f);
    return 0;
}
"#), "true\nfalse");
}

// ── String Interpolation ──────────────────────────────────────────────────────

#[test]
fn test_string_interpolation() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let name = "Tino";
    let n = 42;
    println("Hello ${name}, answer is ${n}!");
    return 0;
}
"#), "Hello Tino, answer is 42!");
}

// ── Control Flow ──────────────────────────────────────────────────────────────

#[test]
fn test_if_else() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let x = 10;
    if x > 5 {
        println("big");
    } else {
        println("small");
    }
    return 0;
}
"#), "big");
}

#[test]
fn test_while_loop() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    var i = 0;
    var sum = 0;
    while i < 5 {
        sum = sum + i;
        i = i + 1;
    }
    println(sum);
    return 0;
}
"#), "10");
}

#[test]
fn test_for_range_exclusive() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    var sum = 0;
    for i in 0..5 {
        sum = sum + i;
    }
    println(sum);
    return 0;
}
"#), "10");
}

#[test]
fn test_for_range_inclusive() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    var sum = 0;
    for i in 1...5 {
        sum = sum + i;
    }
    println(sum);
    return 0;
}
"#), "15");
}

// ── Functions ─────────────────────────────────────────────────────────────────

#[test]
fn test_function_call() {
    assert_eq!(run(r#"
namespace math {
    class Ops {
        fnc add(a: Int64, b: Int64) -> Int64 {
            return a + b;
        }
    }
}
fn main() -> Int64 {
    println(Ops.add(3, 4));
    return 0;
}
"#), "7");
}

#[test]
fn test_recursive_function() {
    assert_eq!(run(r#"
namespace math {
    class Seq {
        fnc fib(n: Int64) -> Int64 {
            if n <= 1 {
                return n;
            }
            return Seq.fib(n - 1) + Seq.fib(n - 2);
        }
    }
}
fn main() -> Int64 {
    println(Seq.fib(10));
    return 0;
}
"#), "55");
}

// ── Arrays ────────────────────────────────────────────────────────────────────

#[test]
fn test_int_array_literal_and_index() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let arr = [10, 20, 30, 40, 50];
    println(arr[0]);
    println(arr[4]);
    return 0;
}
"#), "10\n50");
}

#[test]
fn test_int_array_method_len() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let arr = [1, 2, 3, 4, 5];
    println(arr.len());
    return 0;
}
"#), "5");
}

#[test]
fn test_int_array_push_pop() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    var arr = [1, 2, 3];
    arr = arr.push(4);
    println(arr.len());
    println(arr.last());
    return 0;
}
"#), "4\n4");
}

#[test]
fn test_for_loop_over_array() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let arr = [1, 2, 3, 4, 5];
    var sum = 0;
    for x in arr {
        sum = sum + x;
    }
    println(sum);
    return 0;
}
"#), "15");
}

#[test]
fn test_string_array_index() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let names: Array<String> = ["alice", "bob", "carol"];
    println(names[0]);
    println(names[2]);
    return 0;
}
"#), "alice\ncarol");
}

#[test]
fn test_string_array_for_loop() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let words = ["hello", "world"];
    for w in words {
        println(w);
    }
    return 0;
}
"#), "hello\nworld");
}

#[test]
fn test_string_array_len() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let arr: Array<String> = ["a", "b", "c", "d"];
    println(arr.len());
    return 0;
}
"#), "4");
}

// ── Tuples ────────────────────────────────────────────────────────────────────

#[test]
fn test_tuple_basic() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let p = (10, 20);
    println(p.0 + p.1);
    return 0;
}
"#), "30");
}

#[test]
fn test_tuple_nested() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let t = ((1, 2), 3);
    println(t.0.1);
    return 0;
}
"#), "2");
}

// ── Map ───────────────────────────────────────────────────────────────────────

#[test]
fn test_map_set_get() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let m = @{ "x" => 10, "y" => 20 };
    println(m.get("x"));
    println(m.get("y"));
    return 0;
}
"#), "10\n20");
}

#[test]
fn test_map_len_remove() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let m = @{ "a" => 1, "b" => 2, "c" => 3 };
    println(m.len());
    m.remove("b");
    println(m.len());
    return 0;
}
"#), "3\n2");
}

#[test]
fn test_map_contains() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let m = @{ "key" => 42 };
    if m.contains("key") {
        println("found");
    }
    if m.contains("missing") {
        println("wrong");
    } else {
        println("not found");
    }
    return 0;
}
"#), "found\nnot found");
}

// ── Closures / Lambdas ────────────────────────────────────────────────────────

#[test]
fn test_lambda_basic() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let double = fn(x: Int64) -> Int64 { return x * 2; };
    println(double(21));
    return 0;
}
"#), "42");
}

#[test]
fn test_lambda_capture() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let base = 10;
    let add_base = fn(x: Int64) -> Int64 { return x + base; };
    println(add_base(5));
    return 0;
}
"#), "15");
}

// ── Classes & Methods ─────────────────────────────────────────────────────────

#[test]
fn test_class_fields_and_methods() {
    assert_eq!(run(r#"
class Point {
    x: Int64;
    y: Int64;
    fn sum() -> Int64 {
        return this.x + this.y;
    }
}
fn main() -> Int64 {
    let p = new Point(3, 4);
    println(p.sum());
    return 0;
}
"#), "7");
}

#[test]
fn test_class_inheritance() {
    assert_eq!(run(r#"
class Animal {
    name: String;
    fn kind() -> String {
        return "animal";
    }
}
class Dog extends Animal {
    fn kind() -> String {
        return "dog";
    }
}
fn main() -> Int64 {
    let d = new Dog("Rex");
    println(d.kind());
    return 0;
}
"#), "dog");
}

// ── Interfaces & vtable ───────────────────────────────────────────────────────

#[test]
fn test_interface_dispatch() {
    assert_eq!(run(r#"
interface Shape {
    fn area() -> Int64 { return 0; }
}
class Square implements Shape {
    side: Int64;
    fn area() -> Int64 {
        return this.side * this.side;
    }
}
namespace shapes {
    class ShapeUtils {
        fnc print_area(s: Shape) -> Int64 {
            println(s.area());
            return 0;
        }
    }
}
fn main() -> Int64 {
    let sq = new Square(5);
    ShapeUtils.print_area(sq);
    return 0;
}
"#), "25");
}

// ── Enums & Pattern Matching ──────────────────────────────────────────────────

#[test]
fn test_enum_match() {
    assert_eq!(run(r#"
enum Dir { North; South; East; West; }
namespace dir {
    class DirUtils {
        fnc label(d: Dir) -> String {
            match d {
                North => return "N";
                South => return "S";
                East  => return "E";
                West  => return "W";
            }
        }
    }
}
fn main() -> Int64 {
    println(DirUtils.label(Dir::North));
    println(DirUtils.label(Dir::West));
    return 0;
}
"#), "N\nW");
}

// ── Generics ──────────────────────────────────────────────────────────────────

#[test]
fn test_generic_function() {
    assert_eq!(run(r#"
fn identity<T>(x: T) -> T {
    return x;
}
fn main() -> Int64 {
    println(identity(99));
    return 0;
}
"#), "99");
}

#[test]
fn test_generic_class() {
    assert_eq!(run(r#"
class Box<T> {
    value: T;
    fn get() -> T {
        return this.value;
    }
}
fn main() -> Int64 {
    let b = new Box<Int64>(42);
    println(b.get());
    return 0;
}
"#), "42");
}

// ── Builtins ──────────────────────────────────────────────────────────────────

#[test]
fn test_math_builtins() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    println(abs(-7));
    println(min(3, 5));
    println(max(3, 5));
    return 0;
}
"#), "7\n3\n5");
}

#[test]
fn test_string_methods() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let s = "Hello";
    println(s.toUpper());
    println(s.toLower());
    println(s.len());
    return 0;
}
"#), "HELLO\nhello\n5");
}

#[test]
fn test_string_contains_starts_ends() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    let s = "Hello World";
    println(s.contains("World"));
    println(s.startsWith("Hello"));
    println(s.endsWith("World"));
    return 0;
}
"#), "true\ntrue\ntrue");
}

// ── Try / Catch ───────────────────────────────────────────────────────────────

#[test]
fn test_try_catch() {
    assert_eq!(run(r#"
fn main() -> Int64 {
    try {
        println("try");
    } catch e: Int64 {
        println("catch");
    } finally {
        println("finally");
    }
    return 0;
}
"#), "try\nfinally");
}

// ── Modules / Imports ─────────────────────────────────────────────────────────

#[test]
fn test_multiple_functions() {
    assert_eq!(run(r#"
namespace math {
    class MathUtils {
        fnc square(x: Int64) -> Int64 { return x * x; }
        fnc cube(x: Int64) -> Int64 { return x * x * x; }
    }
}
fn main() -> Int64 {
    println(MathUtils.square(4));
    println(MathUtils.cube(3));
    return 0;
}
"#), "16\n27");
}

// ── Garbage Collector ─────────────────────────────────────────────────────────

#[test]
fn test_gc_many_string_allocations() {
    // 100k string allocations without explicit free — GC must not crash or OOM
    assert_eq!(run(r#"
fn main() -> Int64 {
    var i: Int64 = 0;
    while (i < 100000) {
        let s: String = "item-" + i.toString();
        i = i + 1;
    }
    println("ok");
    return 0;
}
"#), "ok");
}

#[test]
fn test_gc_list_churn() {
    // Repeated list creation — ensures GC collects unreachable list objects
    assert_eq!(run(r#"
fn main() -> Int64 {
    var round: Int64 = 0;
    while (round < 1000) {
        var nums: List<Int64> = [];
        var j: Int64 = 0;
        while (j < 100) {
            nums.push(j);
            j = j + 1;
        }
        round = round + 1;
    }
    println("ok");
    return 0;
}
"#), "ok");
}

// ── Lambda naming across independent generic specializations ──────────────────

#[test]
fn test_independent_generic_specializations_dont_collide_on_lambda_names() {
    // Two DIFFERENT generic classes (Box<T>/Bag<T>), each with its own
    // static factory method containing an inline lambda literal, both
    // specialized for the same T (Person) in the same program. Lambda
    // names used to be derived from `temp_count` (codegen.rs), which is
    // deliberately saved/restored around generic-specialization
    // generation for an unrelated reason (so the CALLER's own SSA temp
    // numbering doesn't skip ahead) -- but that same save/restore meant
    // two independently-generated specializations could start from the
    // same restored baseline and emit an identically-named `__lambda_N`,
    // failing with "internal compiler error: generated invalid LLVM IR
    // ... invalid redefinition of function '__lambda_4'". Found live
    // while building tinox.core:ui's DataGrid<T>/GridColumn<T> widgets
    // (this exact shape: two small generic helper classes, each with
    // their own inline lambda, both specialized for the same T). Fixed
    // with a separate, never-reset `lambda_counter` field. This test
    // must actually RUN (not just compile) and print the right values --
    // a bug in the ret_ty/argument plumbing around the collision could
    // still "fix the redefinition" while leaving the wrong lambda
    // bodies wired to the wrong names.
    assert_eq!(run(r#"
class Person {
    var name: String;
}

class Box<T> {
    var accessor: fnc(T) -> String;
    fnc make(accessor: fnc(T) -> String) -> Box<T> {
        return Box<T> { accessor: accessor };
    }
    fnc makeDummy() -> Box<T> {
        return Box<T> { accessor: fn(item: T) { return "box-dummy"; } };
    }
}

class Bag<T> {
    var accessor: fnc(T) -> String;
    fnc make(accessor: fnc(T) -> String) -> Bag<T> {
        return Bag<T> { accessor: accessor };
    }
    fnc makeDummy() -> Bag<T> {
        return Bag<T> { accessor: fn(item: T) { return "bag-dummy"; } };
    }
}

fn main() -> Int64 {
    let p: Person = Person { name: "Ada" };
    let b1: Box<Person> = Box::make(fn(person: Person) { return "box:" + person.name; });
    let b2: Bag<Person> = Bag::make(fn(person: Person) { return "bag:" + person.name; });
    let d1: Box<Person> = Box::makeDummy();
    let d2: Bag<Person> = Bag::makeDummy();

    let r1: String = b1.accessor(p);
    let r2: String = b2.accessor(p);
    let r3: String = d1.accessor(p);
    let r4: String = d2.accessor(p);
    println(r1);
    println(r2);
    println(r3);
    println(r4);
    return 0;
}
"#), "box:Ada\nbag:Ada\nbox-dummy\nbag-dummy");
}

#[test]
fn test_float64_to_int_method_call() {
    // issue #223: `x.toInt()` on a Float64 receiver -- typecheck already
    // registers `Float64_toInt` as valid (Float64 math methods block,
    // tinox-typecheck/src/lib.rs), but codegen had no matching arm for
    // it in the numeric method dispatch block (only `toString`/`sqrt`
    // were handled), so a typecheck-accepted call fell through to the
    // generic declared-type-mangled-name method call path, which
    // synthesized a call to an undeclared `@Float_toInt` symbol (not
    // even the same mangled name typecheck itself uses) and produced
    // invalid LLVM IR (an ICE) -- for a PLAIN variable receiver, not
    // just an inline expression. `x as Int64` (cast syntax) was the
    // only working float->int conversion until this fix. Covers a bare
    // Float64 variable and an inline Float64 arithmetic expression (the
    // shape that first surfaced this while building tinox-k8s-ui2's
    // Quantity.tnx CPU-quantity parser).
    assert_eq!(run(r#"
fn main() -> Int64 {
    let f: Float64 = 2.5;
    let a: Int64 = f.toInt();
    println(a.toString());

    let b: Int64 = (f * 1000.0).toInt();
    println(b.toString());
    return 0;
}
"#), "2\n2500");
}

#[test]
fn test_gc_objects_survive_collection() {
    // Objects that are still reachable must NOT be collected
    assert_eq!(run(r#"
class Counter {
    var value: Int64;
    fn increment() -> Nothing {
        this.value = this.value + 1;
    }
}
fn main() -> Int64 {
    let c: Counter = Counter { value: 0 };
    var i: Int64 = 0;
    while (i < 10000) {
        let noise: String = "noise-" + i.toString();
        c.increment();
        i = i + 1;
    }
    println(c.value.toString());
    return 0;
}
"#), "10000");
}
