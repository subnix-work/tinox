# Absence and recoverable error contracts

Issue #306, first additive slice. Source inspection is against the assigned
worktree at `e4ea0cc` (compiler 2.4.0), rather than assuming the review's
`044c773` baseline is still current. The locally available `origin/develop`
history through `80b48b1` contains no later absence-contract implementation;
its HTTP request source still has the sentinel getters. Live issue comments
could not be checked because this environment cannot resolve GitHub.

## Contract choice

| Situation | Contract for new library APIs | Caller action |
| --- | --- | --- |
| Lookup can legitimately have no value | `Option<T>` | Check presence; keep `Some("")` distinct from `None` |
| Expected operation can fail | Existing `Result<T>` with a descriptive String error | Check status, preserve diagnostic context |
| Stable error categories or retry metadata are required | Explicit domain outcome object | Inspect documented fields, never parse a diagnostic message |
| Programming/precondition/setup failure | Documented exception or existing hard runtime failure | Catch exceptions at an appropriate boundary; fix invalid setup |
| Operation can fail or succeed with no value | `Result<Option<T>>`, only if both states are meaningful | Handle failure before absence |

`T?` remains available in the language, but new lookup APIs should not offer
both nullable and Option alternatives for the same absence. Do not wrap an
existing ambiguous sentinel in Option or Result: once the underlying cause
has been discarded, a wrapper cannot reconstruct it.

`Result<T>` keeps its String error and generic arity. A general
`Result<T,E>` and allocation changes are deferred to separate work with
concrete consumers and measurements. A domain outcome must carry status and
payload together; avoid adding thread-global "last error" state.

`unwrap` is an explicit choice to throw: Option throws `"Option is None"`,
Result throws the original error. Combinators propagate callback exceptions;
they do not silently turn them into None/Err. Cleanup belongs in `finally`
or an existing explicit close path, including when returning an Err. These
rules do not claim runtime aborts are catchable or change existing exception
semantics.

## Source inventory and migration boundaries

This inventory records the initial migration surface and related public API
families. It is not a claim that all library sentinel contracts have migrated.
Source paths are relative to `crates/` unless marked otherwise.

| Public API / source | Existing behavior | Classification and migration |
| --- | --- | --- |
| `HttpRequest.getHeader` (`tinox-core-ext/http_server`) | `""` for absent or present-empty; exact map spelling first, then case-insensitive scan | Absence: additive `findHeader -> Option<String>` |
| `HttpRequest.getParam` | Case-sensitive map lookup; `""` for absent or present-empty | Absence: additive `findParam -> Option<String>` |
| `HttpRequest.getQuery`, `QueryString::get` | Raw parser; `""` for absent, bare key, or empty value; last duplicate wins, empty keys ignored | Absence: additive `findQuery` / `QueryString::find` |
| `Cache::get`, `LruCache::get` (`tinox-core/.../cache`) | `Option<V>` already | Keep existing explicit absence |
| `Option.unwrap`, `Result.unwrap` | Throw a String on None/Err | Deliberate precondition / exception conversion; unchanged |
| `Stack.pop/peek` (`tinox-core/.../collections`) | Throw `"Stack is empty"` | Existing throwing precondition; optional variants are future work |
| `Env::getVar`, `envGet` (`runtime/runtime.c`) | `""` for unset and present-empty variables | Absence; future optional API must inspect environment presence directly |
| `Regex::findFirst`, `regexFindFirst` (`runtime/runtime.c`) | Public `Regex::findFirst` throws for invalid syntax; raw `regexFindFirst` aborts. Both return `""` for no match or an empty match. | Invalid input is visible; distinguishing absence from an empty match remains future work. See [Regex contracts](regex.md). |
| `Fs::readFile`, `fileReadAllText` (`runtime/runtime.c`) | Open failure returns `""`; reads can return partial text without reporting read/close errors | Expected operational failure; requires a new runtime operation carrying error detail |
| `Fs::writeFile`, `fileWriteAllText` | Open/write/close errors are discarded | Expected operational failure; requires a new checked runtime operation |
| `Fs::stat/isFile/isDirectory/isSymlink/listEntries/rename/deleteDirectory/deleteDirectoryRecursive/realPath/homeDirectory` | Checked additive filesystem APIs; stat returns nullable absence only for ENOENT/ENOTDIR, other failures throw; mutations and enumeration expose errors | Explicit #508 nullable contract; see [filesystem contracts](filesystem.md). Existing empty-only deleteDirectory now throws on failure |
| `Fs::appendFile/copyFile/moveFile` | Compose unchecked reads/writes; move then deletes source | Operational failure and cleanup; do not build Result wrappers on these methods |
| `File::exists`, `fileExists` | Uses `fopen`; false can mean missing or inaccessible | Existing predicate cannot diagnose an I/O failure |
| `Socket` (`tinox-core/.../socket`) | Raw descriptors, `-1`, Bool, or String results | Expected network failure; migrate only with captured OS detail and documented close ownership |
| `Process::run`, `ProcessResult` | Captures stdout/stderr, exitCode, timedOut; documents signal/127 conventions | Existing domain outcome; preserve detail and timeout status |
| `Validation` predicates | Bool answers to validation questions | Intentional predicates; do not turn every false into a Result |

I/O follow-up must capture the operation, path and OS cause at failure time,
close handles on every path, distinguish empty success from failed/partial
reads, and report write/flush/close failures. Copy/move must not delete a
source after a failed destination write. Checking `exists` before reading is
insufficient and introduces a race. No new I/O Result method is shipped in
this slice because the current builtins cannot meet that contract.

## HTTP migration

An annotation-driven handler can treat a missing header separately from an
empty one:

```tinox
import tinox.core.http_server
import tinox.core.option

class TraceController
{
    @GET("/trace")
    fnc trace(@HttpContext ctx: HttpContext) -> HttpContext
    {
        let trace: Option<String> = ctx.request.findHeader("X-Trace")
        if (trace.isNone())
        {
            ctx.response.status(400).text("X-Trace is required")
        }
        else
        {
            // This branch includes a present-empty header.
            ctx.response.text("trace=" + trace.unwrap())
        }
        return ctx
    }
}
```

Import `tinox.core.option` as well as `tinox.core.http_server` when naming
`Option<String>`. For an intentional fallback, migrate
`ctx.request.getQuery("search")` to
`ctx.request.findQuery("search").unwrapOr("")`. To preserve the distinction,
check presence before unwrapping. The optional getters do not validate or
URL-decode values. A bare `?search` is present-empty; a missing `search` is
None. Duplicate query parameters retain the last value.

Existing String getters remain supported with unchanged signatures and
fallbacks. There are no deprecation annotations, warnings, compiler changes,
or LSP changes in this increment. Deprecation requires a released replacement,
migration examples and a separately announced compatibility window.

## Verification and release

`crates/tinox/tests/api_contracts.rs` stages the actual workspace request,
query, JSON, Option and Result sources, avoiding registry installations that
would silently test an older HTTP package. Its fixture checks missing,
empty and nonempty values, header casing and exact-match precedence, query
parser compatibility, legacy getter calls, detailed Result errors, exception
propagation and `finally` cleanup. It does not start a server or add protocol,
spawn/await or timing behavior; wire-parser behavior is outside this slice.

Run `cargo test --release -p tinox --test api_contracts` and the background
`make check` gate in a provisioned environment. Neither can execute here:
`make`, Cargo and Rust are absent. A failed gate is not a green gate; do not
commit or publish this slice until it passes.

No package is published or version bumped here. Before publication, bump the
HTTP package version and add its generated immutable
`docs/tinox-core/http_server/<new-version>/docs.html`; preserve all existing
version pages. The parallel German/English language references describe the
new policy now. See `CLAUDE.md` for the package-doc generation procedure.
