# Tinox Programming Language Specification

**Version:** 2.1.0
**Status:** Actively developed — self-hosting tooling (LSP, `tinox fmt`, package
registry client, dev dashboard) and 70+ documented bugfixes deep, not an early
draft. This document was last verified against the actual compiler/runtime
source on 2026-08-12; where a feature exists as a lexer/parser keyword but has
no real-world usage in the codebase, that is called out explicitly rather than
presented as a working feature.

---

## Overview

Tinox is a statically typed, compiled programming language combining
Java-like class-based syntax with Go-inspired concurrency (`spawn`/`await`)
and a **declarative, annotation-driven design** for boilerplate-heavy
concerns: REST routing, WebSocket/AMQP endpoints, dependency injection,
JSON (de)serialization, and CLI argument parsing all work by attaching
`@Annotation`s to classes/methods/parameters rather than hand-writing
wiring code — see [Annotations](#annotations) below. Programs compile
through a hand-emitted LLVM IR backend to native executables, linked
against a C runtime (`runtime/runtime.c`) that provides a Boehm
garbage collector, real POSIX-thread concurrency, and the HTTP/WebSocket/
AMQP/TLS primitives the stdlib annotations build on.

## Lexical Structure

### Identifiers

Identifiers start with a letter or underscore, followed by any number of
letters, digits, or underscores.

```
identifier ::= (letter | '_') (letter | digit | '_')*
```

### Keywords

```
if          else        while       for         loop        return
break       continue    match       case        class       interface
extends     implements  trait       enum        new         this
super       self        public      private     protected   package
static      final       abstract    override    readonly    var
val         let         const       mut         fn          fnc
throw       throws      try         catch       finally     defer
spawn       channel     send        recv        select      default
async       await       module      namespace   import      export
as          in          where       extern      unsafe      ref
sizeof      typeof      is          cast        null        Nothing
Never       Any         true        false       immutable
```

Several of these are reserved but have little or no real usage in the
codebase today — see the notes under [Concurrency](#concurrency) (`channel`/
`send`/`recv`) and [Trait Declarations](#trait-declarations) (`trait`).
`case` is reserved but `match` arms do not currently use it (see
[Match Expressions](#match-expressions)).

### Literals

#### Integer Literals
```
int-literal ::= decimal | hex | octal | binary
decimal     ::= digit+
hex         ::= '0' ('x' | 'X') hexdigit+
octal       ::= '0' ('o' | 'O') octdigit+
binary      ::= '0' ('b' | 'B') bindigit+
```

#### Float Literals
```
float-literal ::= digit+ '.' digit* exponent?
exponent      ::= ('e' | 'E') ('+' | '-')? digit+
```

#### String Literals
```
string-literal ::= '"' (escaped | character)* '"'
escaped        ::= '\' (n | t | r | \ | " | ' | x hexdigit hexdigit | u '{' hexdigit+ '}' | U hexdigit{8})
```

#### Character Literals
```
char-literal ::= '\'' (escaped | character) '\''
```

#### Boolean Literals
```
true | false
```

### Operators

```
+    -    *    /    %    =    ==   !=   <    >    <=   >=
&&   ||   !    &    |    ^    ~    <<   >>   >>>  ++   --
+=   -=   *=   /=   %=   &=   |=   ^=   <<=  >>=  >>>=
->   =>   ::   ..   ...  ?    ??   ?:   @    ,    .
```

### Whitespace and Comments

- Whitespace (spaces, tabs, carriage returns) is ignored except to separate
  tokens.
- Newlines end statements (see [Statement Termination](#statement-termination)).
- Single-line comments start with `//`.
- Multi-line comments start with `/*` and end with `*/`. The stdlib
  (`crates/tinox-core/**/*.tnx`) uses `/** ... */` as a doc-comment
  convention, but this is stylistic — the lexer treats it as an ordinary
  block comment, not a distinct doc-comment token.

### Statement Termination

A statement ends at the end of its line. There is no statement terminator
character: a `;` that terminates a statement is a hard compile error ("semicolons
were removed"), and several statements cannot share a line.

A statement **continues** on the next line only when

- the line ends with a binary operator, a comma or an opening bracket
  (`(` `[` `{`), or
- the next line starts with `.` (a method chain).

Inside `( )` and `[ ]` newlines are ordinary whitespace. Anything else ends the
statement. In particular a line that starts with `(`, `[`, `-` or `+` directly
after a complete expression is a hard error (the compiler never guesses whether it
continues the previous line or begins a new statement); put the operator at the end
of the previous line, or restructure the code.

```tinox
const total = first +
    second +
    third                  // continues: lines end with an operator

const name = user
    .profile()
    .displayName()         // continues: lines start with '.'

return                     // a bare 'return' ends at its line; the value must
    value                  // start on the same line (this is a separate statement)
```

Specifics:

- `else`, `catch` and `finally` may start a line after the closing `}`.
- The last statement before a closing `}` needs no line break
  (`fn f() -> Int64 { return 1 }`).
- A function signature that is not followed by `{` is an abstract declaration and
  ends at its line (interface and trait methods).
- Enum variants are separated by newlines, one variant per line
  (`enum E { A B }` and `enum E { A, B }` are errors).
- Match arms are separated by a newline or a comma.
- The one remaining `;` is the separator inside a C-style `for (init; cond; update)`
  header, which is not a statement terminator.

### Brace Style

**Allman style is the enforced convention** (`tinox fmt`'s only supported
output style, see [Tooling](#tooling)): the opening brace of a class,
method, `if`, `while`, etc. goes on its own line, not at the end of the
previous line:

```tinox
class Point
{
    var x: Float64
    var y: Float64

    fn distanceTo(other: Point) -> Float64
    {
        if this.x == other.x
        {
            return 0.0
        }
        return 1.0
    }
}
```

Every example in this document follows this style, matching the entire
codebase.

---

## Type System

### Primitive Types

| Type      | Description                       | Size    |
|-----------|------------------------------------|---------|
| Int8      | 8-bit signed integer               | 1 byte  |
| Int16     | 16-bit signed integer              | 2 bytes |
| Int32     | 32-bit signed integer              | 4 bytes |
| Int64     | 64-bit signed integer              | 8 bytes |
| UInt8     | 8-bit unsigned integer             | 1 byte  |
| UInt16    | 16-bit unsigned integer            | 2 bytes |
| UInt32    | 32-bit unsigned integer            | 4 bytes |
| UInt64    | 64-bit unsigned integer            | 8 bytes |
| Float32   | 32-bit IEEE 754 float              | 4 bytes |
| Float64   | 64-bit IEEE 754 float              | 8 bytes |
| Bool      | Boolean (true/false)               | 1 byte  |
| Char      | Unicode scalar value (i32-backed)  | 4 bytes |
| String    | UTF-8 encoded string                | varies  |
| Nothing   | No value (like `void`)              | 0 bytes |
| Never     | Diverging type (a function that never returns) | 0 bytes |
| Any       | Dynamic/root type                   | varies  |

There is **no `Unit` type** — `Nothing` is the "no return value" type
used throughout the codebase (e.g. `fn removeTaskAt(...) -> Nothing`).
`Never`/`Any` exist in the type system and are handled by the type
checker/codegen, but have essentially no real usage in example/stdlib code
today — treat them as available but rarely-exercised.

### Composite Types

```
Array<T>            Built-in array type
List<T>              Growable list (stdlib class, tinox.core.list — the
                      one actually used pervasively for collections)
Map<K, V>            Built-in map type
Ref<T>               Mutable reference to T
(T1, T2, ...)        Tuple type, indexed with .0/.1/...
T?                    Nullable type (parsed and type-checked; new library
                      lookup APIs use Option<T> for absence; legacy APIs
                      may still return sentinels)
Fn(P1, P2) -> R       Function type
```

`List<T>` (not `Array<T>`) is what almost all real code reaches for — see
`.push()`/`.pop()`/`.len()` usage throughout the stdlib and examples.

---

## Declarations

### Variable Declarations

```tinox
// Immutable variable (`let` is still accepted as a deprecated alias for `const`)
const x: Int32 = 42
val y: Int64 = 100

// Mutable variable
var counter: Int32 = 0
counter = counter + 1
```

### Modules, Namespaces, and Imports

Every stdlib file declares a dotted-path module and matching namespace
block up front:

```tinox
module tinox.core.result

namespace tinox.core.result
{
    class Result<T>
    {
        // ...
    }
}
```

Project-local files (examples, application code) typically skip the
`module`/`namespace` wrapper entirely and declare types at the top level
of the file — both forms are valid, and imports work identically either
way:

```tinox
import tinox.core.http_server   // pull in a stdlib module
import tinox.core.json
import User                      // pull in a project-local type
```

**Since 2026-07-26, every `.tnx` file may contain at most one top-level
`class`/`interface`/`enum`.** If it declares one, the file name must match
the type name exactly (`class Player` → `Player.tnx`). Files with no type
at all (plain `fn`/`main` scripts) are unaffected. A directory can hold
multiple such single-type files under one importable module path — one
`import` statement pulls in every file in the directory. Files with no
type (driver/entry-point scripts) keep whatever name they had.

### Static Methods (`fnc`)

Static methods belong to a class but do not require an object instance.
They are declared with `fnc` and called via `ClassName::method(args)`.

`::` is mandatory here, not a stylistic choice: `ClassName.method(args)`
is a compile error that names the correct form. `.` is reserved for calls
through a value (an instance method, or a builtin on a String/List/Map).
Both spellings used to compile identically and silently, which let the two
drift apart inside a single file — `Json::serialize` next to `DB.of`.

```tinox
class Utils
{
    fnc add(a: Int64, b: Int64) -> Int64
    {
        return a + b
    }

    fnc square(x: Int64) -> Int64
    {
        return x * x
    }
}

Utils::add(3, 4)     // 7
Utils::square(5)     // 25
```

### Instance Methods (`fn`)

Instance methods have access to `this` and are called on an object.

```tinox
class Point
{
    var x: Float64
    var y: Float64

    fn distanceTo(other: Point) -> Float64
    {
        const dx: Float64 = this.x - other.x
        const dy: Float64 = this.y - other.y
        return (dx * dx + dy * dy).sqrt()
    }
}
```

### Object Construction

Objects are built with **struct-literal syntax** — field name/value pairs
in braces, not a positional `new ClassName(args)` constructor call:

```tinox
const p: Point = Point { x: 1.0, y: 2.0 }
const user: User = User { id: 1, name: "Alice" }
```

### Entry Point (`class Main`)

**Since 2026-08-09, every program built via `tinox build`/`tinox run`
needs a `class Main { fnc main() -> Int32 { ... } }`** in the entry
file — a plain top-level `fn main()` is no longer the entry point and is
a hard compile error if `class Main` is missing (with one exception:
`main()` no longer even needs to be defined by hand for annotation-only
programs — see [Auto-Run Programs](#auto-run-annotations--auto-run-programs)
below).

```tinox
class Main
{
    fnc main() -> Int32
    {
        println("Hello!")
        return 0
    }
}
```

Exempt from this requirement: `@Command` CLI programs (their own argv
dispatch generates their own entry point) and `tinox test` (its own
test-runner entry). `tinox check` only type-checks and never invokes
codegen, so it is unaffected either way.

### Class Declarations

```tinox
class Circle
{
    var radius: Float64

    fn area() -> Float64
    {
        return 3.14159 * this.radius * this.radius
    }

    fnc unitCircle() -> Circle
    {
        return Circle { radius: 1.0 }
    }
}
```

### Class Inheritance

```tinox
class Animal
{
    var name: String

    fn speak() -> String
    {
        return "..."
    }
}

class Dog extends Animal
{
    fn speak() -> String
    {
        return "Woof!"
    }
}
```

### Interface Declarations

Interface methods are signature-only (no body, the declaration ends at its line) —
there is no support for default method implementations in interfaces
today.

```tinox
interface IDrawable
{
    fn draw() -> Int64
}

class CheckerImpl implements IChecker
{
    fn draw() -> Int64
    {
        return 0
    }
}
```

### Enum Declarations

Variants may carry associated data. Variants are separated by newlines (one
variant per line).

```tinox
enum Direction
{
    North
    South
    East
    West
    Diagonal(dx: Int32, dy: Int32)
}
```

Variant values are referenced either as `EnumName::Variant` or
`EnumName.Variant` — both forms occur in real code.

An enum whose variants all carry no data also has a name and text conversion
built in:

```tinox
enum TaskStatus
{
    QUEUED
    RUNNING
    DONE
}

const s = TaskStatus::parse("RUNNING")   // text -> variant; fails hard on an unknown name
println(s.name)                          // "RUNNING"
const all = TaskStatus::values()         // List<TaskStatus>, in declaration order
```

`value.name` fails hard for a value that is not one of the variants. A variant
that is itself called `parse` or `values` keeps its meaning as a variant. Enums
whose name is declared more than once (two modules) get none of these.
Behind the scenes the compiler synthesizes a hidden `__Enum_<Name>` class.

In an `@Entity`, a field of such an enum type is a text column holding the
variant name (`where` filters compare against the name too):

```tinox
@Entity
@Table("tasks")
class Job
{
    @Id @GeneratedValue
    var id: Int64
    var status: TaskStatus
}
// DB::of(Job).filter(j => j.status == TaskStatus::DONE).count()
```

An unknown name in the column fails hard when the row is read. A `first()`
that finds no row leaves the enum field at 0, which matches no variant.

### Trait Declarations

`trait` is a reserved keyword recognized by the lexer/parser, but has
**zero real usage** anywhere in the current codebase (stdlib, examples,
or tests) — treat it as not yet a working feature rather than something
to reach for.

### Match Expressions

```tinox
match v
{
    Str(s) => return s.len()
    _      => return 0
}
```

Arms are separated by a newline (a `,` is also accepted), use `=>` (not `case`), and `_` is
the wildcard pattern. Arm bodies can also be blocks:

```tinox
match tf[5]
{
    BoolVal(b) => { more1 = b }
    _          => {}
}
```

### Generics

```tinox
class Pair<T, U>
{
    var first: T
    var second: U

    fn new(first: T, second: U) -> Pair<T, U>
    {
        return Pair<T, U> { first: first, second: second }
    }
}
```

A method can introduce its own type parameter independent of its class's:

```tinox
class Box<T>
{
    var value: T

    fn transform<U>(f: fnc(T) -> U) -> Box<U>
    {
        return Box<U> { value: f(this.value) }
    }
}
```

Generic bound syntax (`<T: SomeInterface>` / `where T: X`) is not
exercised anywhere in real code today.

### Absence and recoverable errors

New library contracts use the existing `tinox.core.option.Option<T>` for
an absent value and `tinox.core.result.Result<T>` for an expected failure.
`Some("")` is a present empty string; `None` is absence. An empty value is
not evidence of failure. Use `isSome()`/`isNone()` or `unwrapOr()` to handle
absence deliberately. Do not add a nullable `T?` variant of the same lookup
or mix nullable and optional values without distinct domain meanings.

`Result<T>` has a **String** error today. Check `isOk()`/`isErr()`, then
read the value or `getError()`. Never infer success from an empty error or
parse error wording as a machine-readable code. Preserve operation, target,
and underlying cause in failure messages; redact secrets. An API that needs
stable error codes or retry metadata should define a domain outcome object
with documented fields. This increment does not introduce `Result<T,E>`,
change generic arity, or change the List-backed representation. Typed errors
and representation changes require separate design and allocation evidence.
`Result<Option<T>>` is appropriate only when an operation can independently
fail or succeed without a value.

Exceptions (`throw String`, `catch e: String`) propagate to the caller's
handler and remain appropriate for violated preconditions, programming or
setup failures, and existing throwing contracts. `Option.unwrap()` on None
throws `"Option is None"`; `Result.unwrap()` on Err throws its original
error string. They are explicit conversions to an exception contract.
`map`/`andThen` callbacks can throw; neither container catches callback
exceptions. Use `finally` for required cleanup on normal or exceptional
exit; returning an Err is ordinary control flow and still requires cleanup.
Process aborts and runtime safety checks are not recoverable Result errors.

The first additive migration is `HttpRequest.findHeader`, `findParam`,
`findQuery`, and `QueryString::find`, all returning `Option<String>`.
Headers retain case-insensitive lookup with exact spelling preferred;
path/query names are case-sensitive. Queries keep the existing raw parser:
no percent/plus decoding, last duplicate wins, bare keys are present-empty,
and empty keys are ignored. For example:

```tinox
import tinox.core.http_server
import tinox.core.option

// Inside an annotation-driven route with @HttpContext ctx:
let header: Option<String> = ctx.request.findHeader("X-Trace")
if (header.isNone()) { ctx.response.status(400).text("X-Trace is required") }
else { ctx.response.text("trace=" + header.unwrap()) }
```

`getHeader`/`getParam`/`getQuery` and `QueryString::get` retain their String
signatures and empty fallback. `findHeader("X").unwrapOr("")` deliberately
opts back into that old contract. No compiler/LSP deprecations are added;
replacements must first be released and available to consumers. See the
[API inventory and migration boundaries](api-contracts.md) for legacy I/O
limitations and follow-up work.

### Try-Catch-Finally

Real and commonly used, including `finally` without a matching `catch`:

```tinox
try
{
    throw "x"
}
catch e: String
{
    println("caught: " + e)
}
finally
{
    println("finally block")
}
```

---

## Expressions

### Binary Operators (by precedence)

| Precedence  | Operators                    | Associativity |
|-------------|-------------------------------|---------------|
| 1 (lowest)  | `\|\|`                        | Left          |
| 2           | `&&`                          | Left          |
| 3           | `==`  `!=`                    | Left          |
| 4           | `<`  `<=`  `>`  `>=`          | Left          |
| 5           | `<<`  `>>`  `>>>`             | Left          |
| 6           | `+`  `-`                      | Left          |
| 7           | `*`  `/`  `%`                 | Left          |
| 8 (highest) | unary `-`, `!`, `~`            | Right         |

### Control Flow

```tinox
// If-else
if x > 0
{
    print("positive")
}
else if x < 0
{
    print("negative")
}
else
{
    print("zero")
}

// While loop
var i: Int32 = 0
while i < 10
{
    print(i)
    i = i + 1
}

// For loop
for item in collection
{
    print(item)
}

// Loop (infinite)
loop
{
    if done { break }
}
```

---

## Concurrency

Real concurrency in Tinox is `spawn`/`await`, backed directly by POSIX
threads (`pthread_create` in `tinox_task_spawn`, runtime.c) — genuine
parallelism, not a cooperative/green-thread scheduler. `spawn` is a
**unary expression applied to a call expression**, not a call to a
`spawn(...)` function, and returns a handle used with `await`:

```tinox
const handle: Int64 = spawn fetchData(url)
// ... do other work ...
const result: Data = await handle
```

`async fnc` marks a function as spawnable this way:

```tinox
async fnc fetchData(url: String) -> Data
{
    // ...
}
```

Note the keyword is `async fnc` (static), matching real usage in the
codebase — not `async fn`.

**`channel`/`send`/`recv`/`Channel<T>` are reserved keywords with parser
support but no real usage anywhere in the codebase.** Every concurrency
example in this repo uses `spawn`/`await` directly (commonly with a
shared, GC-managed object for passing results back, not a channel). Do
not assume `Channel<T>` is a working, tested primitive.

---

## Annotations

Annotations are Tinox's primary mechanism for eliminating boilerplate —
the project's own convention (see this repo's `CLAUDE.md`) is to prefer a
declarative annotation over hand-written wiring wherever one exists. A
custom annotation can also be defined as plain library code via
`@annotation class Foo { ... }` (e.g. `@JsonSerializable` itself is
defined this way in `tinox.core.json`, not a compiler builtin).

This is not an exhaustive reference — see `docs.html`/`docs_en.html` (the
built-in annotations table, kept in sync with the compiler) for the full,
current list with parameters. The major built-in groups:

### REST Routing + Parameter Binding

`@GET`/`@POST`/`@PUT`/`@PATCH`/`@DELETE` + `@Path("...")` mark a method as
an HTTP route. Every parameter needs exactly one binding annotation —
`@PathParam`/`@QueryParam` (bind by the parameter's own name, no backward-
compatible "bare `ctx` parameter" shape exists), `@PostParam` (binds the
deserialized JSON body, target type must be `@JsonSerializable`), or
`@HttpContext` (opts into the raw request/response handle). The **return
type decides the response mode**: `-> HttpContext` is manual mode (the
handler builds `ctx.response` itself, e.g. for a dynamic 404 vs 200);
any other type (`@JsonSerializable` class, `List<class>`, String, Int64,
Int32, Bool) is auto-serialize mode — the compiler serializes the
returned value as the JSON response body itself.

```tinox
@GET
@Path("/users/:id")
fn getUser(@PathParam id: Int64, @HttpContext ctx: HttpContext) -> HttpContext
{
    // ... look up id, then either:
    ctx.response.status(404).json("{\"error\":\"not found\"}")
    return ctx
}

@POST
@Path("/users")
@StatusCode(201)
fn createUser(@PostParam user: User) -> User
{
    return user   // auto-serialized as the JSON response body
}
```

A missing/invalid `@PathParam`/`@QueryParam` short-circuits with HTTP 400
+ a JSON error body before the handler ever runs. Also available on
routes: `@Produces("...")`, `@Consumes("...")`, `@Auth("bearer"|"basic")`,
`@OIDCRolesAllowed("role", ...)`.

`@Http3RestController(port, cert, key)` routes the exact same
`@GET`/`@PathParam`/etc. machinery over HTTP/3 (QUIC) instead of plain
TCP — the parameter-binding/auto-serialize codegen is shared between both.

### WebSocket and AMQP Endpoints

`@WebsocketEndpoint(port)` + `@OnOpen`/`@OnMessage`/`@OnClose` methods;
`@Amqp091Consumer(...)`/`@Amqp10Consumer(...)` for AMQP 0-9-1/1.0
consumers. Multiple instances of the same kind (but not
`@Http3RestController`) are allowed in one program.

### Dependency Injection / Component Scope

`@ApplicationComponent`/`@Startup` mark a class as an application-scoped
singleton (one shared instance, via a generated `_di_get()` getter);
`@HttpRequestScoped` marks per-request instances (freshly allocated every
call, never cached).

### Auto-Run Annotations / Auto-Run Programs

A class with `@Http3RestController`/`@WebsocketEndpoint`/
`@Amqp10Consumer`/`@Amqp091Consumer`, or a class with plain `@GET`/etc.
routes, is an "auto-run component" — the compiler spawns it on its own
thread automatically at startup, alongside `class Main.main()` if one
exists. Multiple different auto-run kinds can coexist in the same
program (e.g. a REST controller and a WebSocket endpoint together). Every
compiled program with at least one auto-run endpoint prints a startup
banner (loaded modules, registered endpoints, boot time) unless
`[startup] banner = false` is set in `tinox.toml`.

### Other Built-Ins

`@Command` (generates argv-parsing CLI entry points, exempting the class
from the `class Main` requirement above), `@Test` (`tinox test`),
`@JsonSerializable`/`@DoNotSerialize` (library-defined, `tinox.core.json`),
`@Entity`/`@Table`/`@Column`/`@Id`/`@GeneratedValue` (database mapping),
`@Config`/`@Inject`, `@Log`/`@Timed`/`@Counted`/`@Gauge` (metrics),
`@Sensitive`/`@Masked` (redaction in logs/dumps).

---

## Compiler Version

`tinox.toml`'s `[package]` section must declare `compiler = "<version>"`, the
exact version of the compiler that builds the package. Any other running
compiler (and a missing field) is a hard error; there is one syntax per
compiler. Installed dependencies that declare a different version are rejected.

## Standard Library

The stdlib is split into a **core tier** (`crates/tinox-core`, bundled
with the compiler — collections, strings, math, I/O, etc.) and an
**extended tier** (`crates/tinox-core-ext`, published as independently
versioned packages to `tinox-central`, the project's own package
registry — HTTP/1.1/2/3 servers, WebSocket, AMQP 0-9-1/1.0, JWT/OAuth2/
OIDC, REST client/server, crypto, ZIP, and more). A project declares
extended-tier dependencies explicitly in `tinox.toml`'s
`[[dependencies]]` list.

For the current, authoritative module list with usage examples, see
`docs.html` (German) / `docs_en.html` (English) in the repo root, or
browse the published packages on tinox-central — this spec intentionally
does not duplicate that list, since it is added to frequently and would
go stale here.

---

## Tooling

| Component     | Status     | Notes                                            |
|----------------|-----------|---------------------------------------------------|
| Lexer          | Done       | Full Unicode support, string interpolation        |
| Parser         | Done       | AST, namespace/`fnc`, generics                    |
| Type Checker   | Done       | Classes, interfaces, enums, generics, annotations |
| Code Gen       | Done       | Hand-emitted LLVM IR backend                      |
| Runtime        | Done       | C runtime (Boehm GC, I/O, threading, HTTP/WS/AMQP/TLS) |
| Formatter      | Done       | `tinox fmt` — Allman-style brace output           |
| LSP            | Done       | `tinox-lsp`, Eclipse plugin                       |
| REPL           | Done       | `tinox repl`                                      |
| `tinox doc`    | Done       | Generates per-package HTML docs                   |
| `tinox docker` | Done       | Minimal Docker image build from a project         |
| `tinox dev`    | Done       | Watch-mode rebuild + `tinox-devui` dashboard orchestration |
| Package registry | Done     | `tinox install`/`add`/`package`, tinox-central    |
| Debugger       | Not implemented | No dedicated debugger; runtime bugs are debugged via `gdb`/signal handlers directly on the C runtime |

---

## Future / Known-Incomplete Areas

- `trait` — reserved keyword, unimplemented in practice (no real usage
  found anywhere in the codebase).
- `channel`/`send`/`recv`/`Channel<T>` — reserved keywords with parser
  support, but no real usage; `spawn`/`await` is the concurrency
  primitive actually used and tested throughout the codebase.
- Generic bounds (`<T: X>` / `where` clauses) — parsed in some contexts
  but not exercised in real code.
- `T?` (nullable types) — parsed and type-checked, but most stdlib APIs
  prefer sentinel returns over nullable types in practice.
- Pattern-matching exhaustiveness checking.
- Macros.
