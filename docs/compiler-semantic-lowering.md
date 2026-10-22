# Semantic lowering contracts (#300)

The compiler carries one checked semantic representation into lowering. It keeps
source type identity separate from the LLVM representation and from the physical
word slots used by the runtime. PR #335 introduced this boundary; the completion
migration extends it to generic specialization, ordinary calls, arithmetic,
containers and expression joins.

## Checked boundary

`CheckedProgram` persists `TypeChecker::expr_value_types()` and `SemanticFacts`
together, keyed by the exact ordered AST and assigned node IDs. The normal CLI,
test compiler and REPL pass both exports to codegen. A specialization reuses those
node IDs and substitutes the checked symbolic types using its concrete bindings.
It does not run a second typechecker or infer source types from LLVM strings.

`ValueType` retains primitive widths and signedness, callable signatures,
generic arguments, reference mutability, tuple elements and task result types.
Numeric value compatibility is distinct from callable ABI compatibility and
generic identity. `Cell<UInt8>` and `Cell<Int8>` therefore remain different even
though both store their payload in the same runtime word layout.

`SemanticFacts` provides:

- Resolved calls with owner, dispatch, parameter/result types and generic
  bindings. The receiver is normalized out of instance-call argument contracts.
- Declaration identities with namespace, source identity, owner and member,
  separate from backend linkage spellings. Installed source identities include
  the dependency's versioned path. Native extern declarations share the explicit
  native linkage identity. Read-only `tinox describe` can select a qualified
  candidate; native emission rejects ambiguous non-native linkage.
- Field owner/result contracts and declared field type templates.
- Cast source/target contracts and numeric operand promotion plans.

Receiving annotations can bind otherwise unconstrained factories. Missing
bindings fail in the frontend with an actionable diagnostic. Codegen validates
required bindings and checked call/declaration identity; it never substitutes an
unresolved parameter with a guessed `Int64`.

## Consumers

| Consumer | Checked input and lowering behavior |
| --- | --- |
| Generic classes, functions and methods | Full structural bindings drive body/declaration substitution and specialization keys. Width, signedness, class identity, nested arguments, maps, nullable/reference types, tuples and callable signatures participate in identity. LLVM types are derived only afterward. |
| Direct, instance and interface calls | Checked receiver/dispatch and concrete declaration tables select the implementation; declared parameter contracts drive numeric argument conversion. Return ABIs come from checked results or concrete declarations, never the first argument. |
| Indirect callbacks and callback fields | Callable contracts supply the real return ABI and argument conversions. Missing checked call facts produce a compiler invariant diagnostic. |
| Arithmetic and comparisons | Checked promotion converts both operands as values. Unsigned extension, comparison, division and remainder retain unsigned semantics; zero division fails visibly. Float32/Float64 promotion uses numeric conversions instead of bit reinterpretation. |
| Explicit casts and returns | Semantic source/target types determine signedness, width and integer/floating-point conversion. |
| Fields | Checked field templates are specialized before numeric values are converted and stored; reads restore narrow/float/boolean values from the physical word layout. |
| Lists and maps | Literal and assignment values convert to their checked element/value type before word encoding. Indexing, map get, list first/last and tuple access decode using the checked result type. |
| Conditional and match expressions | Each branch converts to the checked join before storage; the merge decodes that join rather than selecting the last emitted branch's representation. |
| Captures, tuples and task results | Shared word encoding/decoding preserves pointers, Float32, Float64 and narrow integers without invalid-width bitcasts. |
| Channels | `Channel<T>` has an explicit word-handle ABI, distinct from a class pointer; send/recv convert and decode the checked element type. |

Mixed numeric values retain the language's existing promotion policy: identical
types preserve their type, differing integer types promote to Int64, and mixed
floating-point/numeric types promote to Float64. Implicit conversion of a numeric
value does not adapt a callback signature or mutate an existing generic object.

## Context that remains intentionally in emission

The concrete `this` layout, inheritance implementation tables, field offsets,
closure environment slots and runtime ABI signatures are emission context, not
alternative semantic type inference. Checked expression types take precedence
over legacy marker propagation. Legacy inference helpers serve synthetic or
unchecked internal ASTs and explicitly dynamic `Any` paths; they do not choose
generic bindings for checked programs.

The runtime's map-key representation remains canonicalized by its existing key
API. Erased native builtin `Fn`/`Any` signatures are explicit dynamic contracts,
not invented concrete source types. Task ownership, cancellation, deadlines and
handle APIs are tracked separately in #301; this migration integrates its
checked `Task<T>` annotations and preserves the pthread/word ABI.

## Verification

Semantic unit tests cover exact type roundtrips, checked call/field/cast/numeric
contracts, full nested generic returns and qualified declaration identities.
The CLI composition tests use the real LLVM verifier and native backend in
debug, release and checked modes. They combine callbacks, interfaces, unsigned
and signed narrow fields, Float32 captures/containers, nested generic factories,
ordinary numeric calls and conditional joins. Invalid callback signatures,
generic values and unresolved/conflicting bindings fail before native tools are
available. Typed task results run 20 times per mode. The complete `make check`
gate includes Clippy, all unit/integration suites, dogfood/jgrep-tinox and grammar
synchronization.
