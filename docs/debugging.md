# Debugging Tinox programs

Build with `tinox build --debug` and debug with gdb 14 or newer. Everything
below is verified by `crates/tinox/tests/debug_info.rs` and
`crates/tinox/tests/dap_launcher.rs`.

| Capability | Status |
|---|---|
| Breakpoints on any source line, step over / into / out | Works: every statement has a `DILocation` (issue #498). |
| Backtraces | Function names with arguments and source lines, e.g. `Main.work (n=4) at Main.tnx:10`. |
| Arguments and locals | Works in `--debug` builds (#499): integers, floats, `Bool`, `Char`, `String` (as text), `this` in methods. Release builds describe functions only. |
| Classes, lists, maps | Works in `--debug` builds (#500): fields at their real offsets, `List<T>` as `{len, cap, data}`, `Map<String, V>` as its entry table. With `runtime/tinox-gdb.py` they print as `{id = 7, label = "seven"}`, `{1, 2, 3}`, `{["ann"] = 31}`. |
| Debug Adapter Protocol | `tinox dap` (#501), see below. |

## Using it

```sh
tinox build --debug -o app
gdb -q app
(gdb) source /usr/share/tinox/tinox-gdb.py      # or runtime/tinox-gdb.py in a checkout
(gdb) break Main.tnx:12
(gdb) run
(gdb) info locals
```

## DAP clients: `tinox dap`

`tinox dap` replaces itself with `gdb --interpreter=dap` and the Tinox
defaults (`debuginfod` off, no pagination, the pretty-printers loaded, and
one-line value summaries for the variables pane). Configure your client with
the adapter command `tinox dap` and a `launch` request whose `program` is the
`--debug` executable:

```sh
tinox dap --check               # gdb version, printer script, the exact command
tinox dap --launch-json ./app   # adapter command + launch request as JSON
```

DAP order matters with gdb: set breakpoints after the adapter's `initialized`
event, then send `configurationDone`; the `launch` response arrives after
that. A breakpoint is reported as pending until the program is loaded and
resolves when it starts.

## Known limits

* Variables are scoped to the function, not the block: a name declared in a
  nested block is visible (uninitialised) before its declaration.
* A generic class instantiation (`Box<Int64>`) is described as a plain pointer.
* Enums appear as integers; `Char` is shown as its code point.
* `spawn`ed tasks are ordinary pthreads without names; the runtime's worker
  pool shows as extra threads.
* `scripts/debug-probe.sh` reports the debug-info coverage of a small sample.
