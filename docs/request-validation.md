# Recursive request DTO validation

`@PostParam` handlers validate raw JSON before constructing the DTO or calling
application code. Validation annotations are built into the compiler and apply
to fields of `@JsonSerializable` classes. Database `@NotNull` retains its database
column meaning; it does not make an HTTP request field required.

```tinox
// Address.tnx
import tinox.core.json
@JsonSerializable
class Address {
    @Length(1,80)
    var city: String
}
```

```tinox
// CreateUser.tnx
import tinox.core.json
import Address
@JsonSerializable
class CreateUser {
    @Required
    var externalId: Int64
    @Length(1,100)
    var name: String
    @Range(0,150)
    var age: Int32
    @Length(1,10)
    var addresses: List<Address>
    @Optional @Length(1,200)
    var note: String
}
```

```tinox
// UserController.tnx
import tinox.core.http_server
import CreateUser
class UserController {
    @POST @Path("/users") @Consumes("application/json")
    fn create(@PostParam user: CreateUser) -> CreateUser {
        return user
    }
}
```

The application still needs its normal `Main.tnx` bootstrap.

| Annotation | Request behavior |
| --- | --- |
| `@Required` | Field must be present and non-null. Empty strings and empty collections remain valid unless constrained by `@Length`. |
| `@Optional` | Missing or explicit null is allowed. A present non-null value must have the declared JSON type and satisfy its constraints. |
| `@Length(min,max)` | Inclusive bounds; strings count Unicode scalar values, lists count elements. Bounds are nonnegative integer values. Implies required unless combined with `@Optional`. |
| `@Range(min,max)` | Inclusive finite numeric bounds, intersected with the declared numeric type's representable domain. Negative integer/float annotation literals are supported. Implies required unless combined with `@Optional`. |

An unannotated field retains legacy optional/default binding behavior, but a
present non-null value must have the correct JSON type. `@Required` and
`@Optional` cannot be combined. Duplicate annotations, invalid argument types,
reversed bounds and constraints on incompatible field types are compile errors.
Integer bounds are compared exactly, including values above 2^53.

Inheritance includes parent fields and their constraints. A same-named child
field replaces the inherited field's entire constraint set; constraints are not
merged. Optional nested objects can terminate recursive DTO graphs with absence
or null. Generated JSON serializers emit null for null nested objects instead
of dereferencing them. Presence is validated before binding; handlers still
receive ordinary DTO fields, not a new presence-tracking representation. In
particular, missing/null optional scalar fields use the existing binder defaults.

## Errors and bounds

The first validation failure returns HTTP 400 with JSON content type:

```json
{"error":"validation_failed","details":[{"path":"$.addresses[0].city","code":"length"}]}
```

Errors are deterministic: raw syntax is checked first, then inherited fields,
child fields in declaration order and collection items in index order. String
map keys are checked in lexicographic order; their paths use JSON bracket
notation, for example `$.labels["region"]`. The response intentionally reports
one failure, not an unbounded list.

| Code | Meaning |
| --- | --- |
| `required` | Required field is absent. |
| `non_null` | Required field or collection element is explicitly null. |
| `type` | Present value has the wrong JSON kind; no string-to-number coercion. |
| `length` / `range` | Declared inclusive constraint or numeric domain failed. |
| `malformed` | Invalid JSON, UTF-8, escape, embedded NUL or trailing input. |
| `duplicate` | Duplicate object key. |
| `number` | JSON numeric literal overflows/underflows its runtime representation or is not finite. |
| `depth` / `size` | A validation resource limit was exceeded. |

Validation limits are **1 MiB of body bytes**, **32 nested JSON value levels**
and **100,000 parsed values**. These limits apply before binding, independently
of a transport's larger configurable request limit. Strings reject escaped NUL
because Tinox's ordinary String ABI is NUL terminated. TCP, HTTP/2 byte adapters
and HTTP/3 body adapters retain exact lengths so raw NUL cannot hide trailing
bytes from DTO validation. Arbitrary manually transformed String bodies have the
ordinary String ABI's limitations.

Supported DTO shapes match the existing JSON binder: scalar strings, booleans
and numeric fields; nested `@JsonSerializable` objects; lists/arrays of strings,
integers or serializable objects; and `Map<String,String>`. Recursive objects
and lists of recursive objects are supported within the depth bound. The
frontend rejects reachable request fields with unsupported JSON binder shapes,
including nullable field types, lists of floats/booleans/nested containers,
non-string maps and ordinary non-serializable classes. Diagnostics name the DTO
field path. Use `@Optional` for request presence rather than a nullable DTO field.
This is a JSON-binding restriction, not a restriction on nullable Tinox syntax
elsewhere. Supporting more binder shapes is separate work.

The strict JSON integer representation is signed 64-bit. Therefore a `UInt64`
request field accepts **0 through 9223372036854775807**, not the entire unsigned
64-bit range. Narrow integer fields and collection elements enforce their own
signedness and width. Float32 fields reject finite Float64 values that would
become infinity when narrowed. NaN and Infinity are not JSON numbers.

## OpenAPI

`tinox contract Main.tnx` exports request-specific schemas under reserved dotted
component names such as `tinox.request.CreateUser`. Dots prevent collisions with
real Tinox class identifiers. Response schemas keep their original class names.
Request schemas include inherited required fields, nullable optional fields,
`minLength`/`maxLength` or `minItems`/`maxItems`, exact numeric
`minimum`/`maximum`, nested request references and the HTTP 400 error structure.
Length bounds are integers. Only reachable request DTOs get request components.
Published, immutable package documentation is not overwritten by this compiler
feature; no package release is needed for the annotations themselves.
