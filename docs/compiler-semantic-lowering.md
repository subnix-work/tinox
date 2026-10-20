# Semantic lowering migration (#300)

This is the first migration slice of #300. The issue remains open: the frontend
now preserves more semantic information, and selected consumers use it directly.
The remaining emission paths are inventoried below rather than removed before
their required data exists.

## Current contract

`TypeChecker::expr_value_types()` remains the single per-expression type channel.
It is keyed by the node IDs assigned after import resolution and accessor
synthesis. `ValueType` now distinguishes integer width and signedness, Float32
from Float64, complete callable signatures, reference targets/mutability, tuple
elements, generic arguments and task result types. Int and Float denote the
language's default Int64 and Float64; they no longer stand for every numeric
width. Generic equality includes arguments. Ordinary numeric value compatibility
is separate from function ABI compatibility and generic identity.

`semantic::SemanticFacts` adds explicit contracts for resolved calls, fields and
casts. Calls retain the checked owner/emission symbol, dispatch kind (direct,
instance, interface, closure or dynamic), parameters and result. Field facts
retain owner, member and result. Cast facts retain source and target types.
These facts and expression types are persisted together in `CheckedProgram`,
under the existing exact-AST/node-numbering cache key. The compiler hands both to
codegen in normal builds and test builds. They are not reconstructed by parsing
rendered API descriptions or by independently type-checking a second AST.

Callable arguments now receive contextual parameter types before body inference;
block lambda returns contribute their actual result. An explicit receiving
annotation can bind a generic factory with no constraining value arguments. Known callback calls check
arity and argument types. Static class calls apply the same compatibility check.
Function signature identity does not silently adapt a callback with a different
parameter ABI. Unannotated lambdas can explicitly discard a result when their
context requires Nothing, retaining the existing closure convention.

## Consumers migrated in this slice

| Consumer | Authoritative input | Behavior |
| --- | --- | --- |
| Local and indirect callback calls | Resolved call signature/result | Emit the actual return ABI and numeric argument conversions; missing checked call data produces an internal-invariant diagnostic. |
| Callbacks in generic method bodies | Declared signature after existing monomorphization | Use the substituted signature for parameter ABI, including callbacks captured by a nested lambda. |
| Explicit numeric casts | Cast source/target facts | Select signed versus unsigned extension and integer/float conversion. |
| Numeric returns | Declared return representation and source type | Convert numeric values instead of emitting mismatched LLVM return types. |
| Narrow typed field reads | Checked field result and verified physical layout | Restore the semantic width from the existing uniform word slot. |
| Tuple elements | Checked element type | Encode and decode words for float, pointer and narrow values. |
| Task results | Checked task/result type | Decode the existing pthread result word, including Float32, Float64, narrow integers and pointers. No new task lifecycle/cancellation API is introduced. |
| Closure environments | Substituted captured-variable representation | Encode words and restore the actual local type; do not store an eight-byte word into a four-byte Float32 alloca. |
| Generic callback inference | Structural callable/generic type | Preserve nested return arguments instead of treating every callback as an opaque Fn. |

`Any` remains explicit. The `Fn` category in polymorphic runtime builtin
signatures is deliberately erased; it is distinct from a declared
`fnc(A) -> B`, which now carries its signature. Raw class annotations/receiver contracts explicitly erase arguments and
still accept different instantiations of that class. An explicitly parameterized
`Box<String>` does not accept `Box<Int64>` merely because the base names match.

Bool field reads now retain their Bool type: printing a Bool field produces
`true`/`false`, consistently with a Bool-returning interface call. Structured
frontend diagnostics likewise report the declared width (for example Int32
instead of the previously erased Int64).

## Audit of remaining context and fallbacks

| Location in codegen.rs | Classification | Remaining work |
| --- | --- | --- |
| `This`, `ctx.current_struct`, specialized field layouts | Legitimate emission context | Keep the concrete class specialization while the frontend checks its symbolic generic body. |
| `infer_struct_type_local` and let/var marker propagation | Mixed | Some receiver/layout context is legitimate; erased Any/container and generic-return cases still need consumer-specific migration before deleting these arms. |
| `marker_to_type`, generic name mangling and binding tables | Missing semantic identity | Existing specialization keys are based on LLVM representations. Complete semantic keys, nested identities and signedness-aware substitutions require a separate slice. |
| `infer_own_type_params`'s final Int64 fallback | Missing authoritative specialization binding | Replace only after every generic callback/body shape exports resolved bindings; it is not presented as an established concrete type here. |
| Non-closure MethodCall/static emission | Mixed | Resolved owner/dispatch facts are exported, but several consumers still use the existing method tables and marker paths. Migrate individually, preserving interface dispatch and specialization context. |
| Arithmetic and implicit conversions outside migrated call/return/cast paths | Missing explicit conversion plan | Continue replacing LLVM-type heuristics with checked conversion plans, including mixed signedness and floating-point promotion. |
| Namespace/package-qualified declaration IDs | Missing semantic identity | Current facts retain the checked class/member and emission key. Package-qualified stable symbol IDs remain future work. |
| Map key type and raw runtime handles | Deliberately erased or incomplete, depending on origin | Maps retain the existing canonical runtime-key contract. Typed task results do not settle ownership, cancellation, deadlines or handle API design (#301). |
| Opaque builtin callback contracts | Deliberately dynamic | Keep erasure explicit and add typed builtin contracts as their consumers migrate. |

This slice does not claim that every i64 fallback is removed or that #300 is
complete. It establishes checked contracts and removes the reproduced failure
paths in #333 while retaining existing #61/#62/#64 composition behavior.

## Verification

- Structural type roundtrips retain widths, signedness, signatures, nested generic
  arguments, reference targets and tuple elements.
- Semantic contract tests compare call results with the expression type export
  and check interface owners, callback parameters, fields and cast types.
- `semantic_composition` combines returned/captured callbacks, generic factories,
  interface dispatch, UInt8 fields, Float32 captures, numeric conversions and
  mixed tuple elements.
- The CLI integration test builds and runs that composition in debug, release
  and checked modes, using the real LLVM verifier and backend.
- Known invalid callback and generic compositions fail in the frontend, even
  with an empty PATH and therefore no native compiler tools available.
- Typed task results run 20 times in each of those three modes. The runtime word
  ABI and real-thread implementation are unchanged.
- Every commit is gated by the complete background `make check`, including
  clippy, unit/integration tests, dogfood and grammar sync.
