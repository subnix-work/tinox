# Debugger support: findings and plan (issue #497)

`scripts/debug-probe.sh` builds a small program with `tinox build --debug` and
reports what a debugger can do with it. Findings with gdb 18.1 (no lldb was
available on the test machine) on x86-64 Linux:

| Capability | Today |
|---|---|
| Debug info present | Yes: one `DW_TAG_compile_unit` and one `DW_TAG_subprogram` per user function (`Class.method`, e.g. `Main.work`), see issue #114. |
| Break on function name | Works (`break Main.work`). The address is the function entry, not the first statement. |
| Break on `file:line` | Only the line of a function's declaration resolves (`Main.tnx:15` for `main`). Lines inside a body do not, because `.debug_line` has one row per function and none per statement. |
| Backtrace | Function names for user frames and the runtime (`tinox_main`, `main`); no line numbers beyond the function line. |
| Step over / into | Not usable: "Single stepping until exit from function ..., which has no line number information". |
| Locals / arguments | None: no `DW_TAG_variable`/`DW_TAG_formal_parameter`, so `info locals` prints "No locals." |
| Types, strings, lists | No `DW_TAG_*_type` entries at all. `String` is a plain `char*`; `List`/`Map`/classes are runtime heap layouts that need pretty-printers. |
| Threads | The runtime starts a worker pool (15 threads in the sample); `spawn`ed tasks are ordinary pthreads without names. |
| Protocol | `gdb --interpreter=dap` (built into gdb 14+) completes `initialize`, `launch`, `setFunctionBreakpoints`, `configurationDone` and reports a `stopped` event with reason `breakpoint`. Set breakpoints after the adapter's `initialized` event. |

## Decision: go, with gdb's native DAP and richer DWARF

Use `gdb --interpreter=dap` as the debug adapter instead of writing one. The
IDE (or any DAP client) speaks standard DAP to it; the gaps above are all on
the compiler side and are what makes the debugger worth having:

1. **Statement-level line table** (`!DILocation` on instructions): step over/in/out, breakpoints on any line.
2. **Variables**: `DILocalVariable` + `llvm.dbg.declare` for parameters and locals of primitive type.
3. **Types and pretty-printers**: `DICompositeType` for classes and fields, and a gdb Python pretty-printer script for `String`, `List`, `Map` (the heap layouts documented in `runtime/runtime.c`).
4. **`tinox dap` launcher**: execs `gdb --interpreter=dap` with the Tinox pretty-printers pre-sourced and the right default settings (`debuginfod` off, no pagination), so clients configure one command.
5. **lldb-dap** as an alternative only if gdb proves insufficient; not needed for the first version.

The IDE side (breakpoint gutter, variables/stack views, debug console) is
tracked in the `tinox-ide` repository and depends on items 1 to 4.
