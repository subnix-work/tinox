# Independent language-semantics matrix

Issue #518 adds four deliberately small composition contracts to the larger
`e2e` and `semantic_lowering` suites. Expected output is specified below before
comparison with native execution; it is not a captured compiler snapshot. The
oracle is the explicit evaluation of these language rules, including the existing
regression contracts in issues #193, #454, #513, #564 and #565.

Run the matrix with the freshly built Cargo compiler:

```sh
cargo test --release -p tinox --test semantic_matrix
```

`CARGO_BIN_EXE_tinox` selects the compiler built for that invocation. Each of the
four fixtures builds separately with `--debug`, default release, and `--checked`.
These are generated-program modes, independent of Cargo's own release profile.
The harness uses isolated output/cache paths, captures stdout and stderr
separately, checks exit status zero, requires empty stderr, and compares the
entire stdout including its final newline. Builds have a 60-second deadline;
normal executions have a 3-second deadline. Expiry kills the process group and
is an explicit failure, even when no output was produced. A separate harness
check contrasts an empty successful process with a silent infinite loop.

## Contracts and independently calculated outputs

### `jumps`

`continue` advances the innermost loop; `break` exits only that loop. A C-style
loop's update runs after `continue`, and an omitted condition is true. The
list sum excludes 1 and stops before 4: `0 + 2 + 3 = 5`. Three outer iterations
each visit inner values 0 and 2, contributing six; only outer values 0 and 2
add ten, hence `6 + 20 = 26`. A while loop increments before its jump and prints
only 1 and 3. String iteration yields Int64-valued UTF-8 bytes, not Unicode
characters: `A` is 65 and `é` is `0xC3 0xA9`, hence 195 and 169.

Complete output: `5, 26, 1, 3, 65, 195, 169`, one value per line.

### `unwind`

A pending return evaluates to 7, runs nested cleanup inside-out, then returns.
A throw from a catch replaces the previous exception but still executes that
catch's enclosing finally before reaching the caller. The caller's catch prints
`second`, then its finally executes. Both a continue at 0 and a break at 1 run
the loop body's finally; neither reaches the `unreachable` print.

Complete ordered lines:

```text
return inner
return outer
7
throw cleanup
second
catch cleanup
0
1
done
```

### `aliasing`

List assignment and passing a list as an argument preserve shared reference
identity. Appending 9 through the parameter makes both aliases length 3;
writing 7 through the second alias changes the first's element zero. Class
assignment also preserves reference identity, so writing 6 through `same`
changes `cell.value`.

Complete output: `3, 7, 9, 6`, one value per line.

### `typed_calls`

Unsigned widening zero-extends: UInt8 255 becomes Int64 255. Signed widening
sign-extends: Int8 -1 remains -1. Unsigned integer division truncates toward
zero, so `255 / 2 = 127`; unsigned comparison `255 > 2` is true. A generic
`Cell<Float64>` passes 1.5 to its callable and stores the returned Float64 2.0.
The generic static callable receives 2.5 and returns 3.0. Tinox's existing
float printing convention omits a redundant decimal fraction.

Complete output: `255, -1, 127, true, 2, 3`, one value per line.

## Mutation proof

The same command always runs two reproducible negative controls. It first builds
and checks the unmodified debug executable for each relevant fixture, then
copies its emitted textual LLVM IR into a temporary mutant. A local `llc`
wrapper substitutes only that temporary generated IR during a fresh debug
build. The real compiler source and repository files remain unchanged. Mutants
must compile and link successfully; a compiler/tool failure is not counted as
detection. A marker proves the wrapper actually applied the replacement, and
all temporary files are removed by the work-directory guard.

1. **Control-flow regression:** redirect the first for-in `continue` edge from
   `for_advance` to the corresponding `for_cond`. This deliberately restores
   #513's missing-advance behavior. The iteration gets stuck at 1 without
   emitting output. The unchanged output oracle must reject it, and the
   harness specifically requires classification as a timeout (300 ms).
2. **Type-lowering regression:** replace the first `zext i8` with `sext i8`.
   The unsigned 255 then widens to -1. The mutant must terminate successfully,
   print -1 as its first line and fail the unchanged output oracle. Changing
   an expected-output file is never used as a mutation.

Mutation injection uses debug mode so the compiler hands textual IR directly
to llc before optimization; all three modes still run the positive contracts.
Mutation selectors assert their target exists, so an IR naming/layout change
requires updating this narrowly scoped injection instead of silently passing.

## Scope and limits

This finite matrix covers these combinations, not every language construct or
numeric boundary. It avoids undefined overflow, unspecified map ordering,
network services, installed packages, and concurrency. It is not a reference
interpreter, exhaustive proof, benchmark, sanitizer run, or fuzzing campaign.
Checked mode exercises Tinox's heap-kind checks, not AddressSanitizer. Existing
minimal e2e regressions remain in place; these four contracts complement them.
The process-group helper targets the Unix platforms already used by the
compiler/runtime. LLVM tools and the normal native link dependencies are
required; absence fails visibly rather than skipping coverage.
