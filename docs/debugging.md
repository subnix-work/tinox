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
| Block scopes, generics, enums (#556) | Works in `--debug` builds: a local is visible from its declaration to the end of its block (a name reused in a sibling block is a separate variable), `Box<Int64>`/`Box<String>` print with their concrete field types, enums as `Color::Green` / `Shape::Square(3, 4)`, `Char` as `'x' (120)`. |
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

* Scopes follow `{ }` blocks and declaration order. Tinox rejects shadowing, so a
  name only repeats in sibling blocks. A loop variable and a `catch` parameter
  are not described yet.
* Only instantiations the program actually creates carry a layout; a generic
  type that never gets an instance stays a plain pointer. Fields inherited from
  a generic base are not expanded.
* Enums are described by their discriminators: without `tinox-gdb.py` `print c`
  still names a payload-free variant (`Green`), and a payload variant shows the
  raw heap address (`print/d` shows the stored 8-byte value either way).
  Variants of different enums with the same name keep separate discriminators.
  Payload slots print as integer, string, float or a raw pointer; classes and
  lists inside a payload are shown as addresses.
* A nullable value is described as its non-null type: `null` is the zero
  pointer (`0x0` for a `String`, `null` for a class or list).
* `spawn`ed tasks are ordinary pthreads without names; the runtime's worker
  pool shows as extra threads.
* `scripts/debug-probe.sh` reports the debug-info coverage of a small sample.
