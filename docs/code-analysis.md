# Complexity analysis (`[analysis]`)

The compiler measures every function and method of the project's own sources and fails the
build when one exceeds a configured limit. It runs in `tinox build`, `tinox run` and
`tinox check` (and therefore in the language server's check path), right after the checked
exceptions pass. Installed dependencies (anything under a `.tinox` directory) and files
outside the project root are never measured.

## Configuration

```toml
[analysis]
max_cyclomatic = 15        # default
max_nesting = 4            # default
max_function_lines = 80    # default
max_params = 7             # default
level = "error"            # default; "error" | "warn" | "off"
```

- A project with a `tinox.toml` but **no** `[analysis]` section gets the defaults at level
  `error`. Without a `tinox.toml` nothing is checked.
- `warn` prints the findings to stderr and lets the build pass; `off` disables the pass.
- An unknown key, a non-positive or unparseable limit, or an unknown level is a hard error.

## Metrics

| Metric | Counts |
|---|---|
| cyclomatic complexity | 1 + each `if`, `while`, `for`, `match` case (+1 more for a guard), `catch`, `select` arm, and each short-circuit `&&` / `||` |
| nesting depth | deepest chain of nested `if`/loops/`match`/`try`/`select`/lambda; an `else if` chain does not nest |
| function lines | from the declaration's first line to the closing brace |
| parameters | declared parameters |

Lambdas count towards their enclosing function. `extern fn` declarations are never measured
(no body; the parameter list is dictated by the C ABI). Compiler-synthesized helper classes
(`__Enum_*`) are skipped.

Function length is approximate: spans carry only a start position, so the length runs to the
starting line of the last statement plus the closing brace. A trailing multi-line construct is
undercounted slightly.

## Opting out: `@AllowComplexity`

```tinox
@AllowComplexity("protocol state machine, split tracked in #123")
fnc dispatch(...) -> ...
```

On a method it exempts that method, on a class every method of the class. The optional string
is the reason. Prefer raising a limit project-wide in `[analysis]` only when the whole code base
genuinely needs it; prefer `@AllowComplexity` for individual, justified exceptions.

## Diagnostics

Findings are `TINOX-COMPLEXITY` diagnostics (the first as the primary one, the rest as
`related` locations) and appear in `--message-format=json` like every other diagnostic.

Implementation: `crates/tinox-typecheck/src/complexity.rs` (pure analysis),
`check_complexity` / `read_analysis_config` in `crates/tinox/src/main.rs`.
