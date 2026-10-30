# Checked exceptions: `@Error`, `@Throws` (design proposal)

Status: **design for review — enforcement is not implemented.** What exists today
(issue #524): `@ErrorMap` and the structured HTTP error envelope (stage 1), and the
*declaration-level* surface of `@Error` and `@Throws` (they parse and are validated, they
change no runtime behaviour). Nothing below under "Proposed" is in the compiler yet.

## Goal

Unlike Java there are **only checked exceptions**: every failure a function can cause must
be visible in its signature, and the compiler refuses code that neither handles nor
declares it. There is no unchecked `RuntimeException`.

## What exists now

```tinox
@Error("USER_NOT_FOUND", 404)          // code, optional HTTP status (400-599)
class UserNotFound { var message: String }

class UserService {
    @Throws(UserNotFound)              // lists the error classes the method may throw
    fn find(id: Int64) -> User { ... }
}
```

Validated today: `@Error` classes need `var message: String`, an `UPPER_SNAKE_CASE`
code unique in the program, and a status in 400–599; `@Throws` may only name `@Error`
classes, each at most once, on methods only. A status on `@Error` also feeds the
`@ErrorMap` table, so an uncaught `"USER_NOT_FOUND: ..."` string already becomes a 404 with
the standard envelope. The annotation is spelled `@Throws` (annotations are capitalised).

## Proposed (not implemented)

### 1. Throwing and catching typed errors

- `throw UserNotFound { message: "user 5" }` is allowed for `@Error` classes (today only
  `String` is throwable, #436). A plain `throw "text"` is no longer allowed in checked code.
- `catch (e: UserNotFound) { ... }` matches by class; several `catch` clauses are tried in
  order and **only the first matching one runs** (today every clause runs).
- Representation: a thrown error stays one `i64` in `__tinox_err`. To keep every existing
  runtime path (tasks, scopes, `defer`, transactions, the HTTP boundary) unchanged, the value
  is the stage-1 string `"CODE: message"`; `catch (e: T)` compares the code prefix and builds
  `e` from the message. The code is therefore the class identity, which is why it must be
  program-unique.

### 2. Enforcement (the checker)

For every call `f(...)` to a function with `@Throws(A, B)`:

1. each of `A`, `B` must be **caught** by an enclosing `try/catch` (a clause whose type is
   `A` or a supertype once error inheritance exists), **or**
2. be listed in the *caller's* own `@Throws`.

Further rules:

| Case | Rule |
|---|---|
| `throw X {...}` inside a method | `X` must be handled locally or listed in that method's `@Throws`. |
| Lambdas / closures | A lambda inherits no list: errors thrown inside must be caught inside, unless the lambda type declares them (later, `fn() -> T throws A`). |
| Interfaces / overriding | An override may list the same or fewer errors than the interface method, never more. |
| `spawn` / `await` | A spawned function's `@Throws` surfaces at `await` (it already re-throws there). |
| `@GET`/`@POST` handlers | May list `@Throws`; each listed class with a status maps to that status through the envelope. Unlisted/unknown failures keep the safe `500 INTERNAL`. |
| `main` | May not leak an error: it must handle everything or list it (a leaked error is a nonzero exit as today). |
| Unused declarations | `@Throws(A)` on a method that cannot throw `A` is a warning (stays correct for interface methods). |

Diagnostics name the call site, the missing error class, and the two fixes (catch it / add it
to `@Throws`), in the existing diagnostic envelope.

### 3. The open decision: implicit failures

Java's "runtime exceptions" correspond to failures that today throw implicitly. The
requirement "no runtime exceptions" forces a decision per class of failure — please choose:

| Failure today | Option A (recommended) | Option B |
|---|---|---|
| Array/String index out of range, `Map` strict get | Hard abort (programming error, not catchable) | Typed `@Error` thrown, forcing `@Throws` everywhere |
| `Option.unwrap()` on None, `Result.unwrap()` on Err | Abort; use `match`/`orElse` for control flow | Typed error |
| Integer overflow / division by zero | Abort (or wrapping per type, as already specified) | Typed error |
| Task cancellation, deadline, shutdown | Stays a **separate, non-catchable control signal** (it already must not become a 500) | — |
| Native/IO failures (`Fs`, sockets, DB) | Typed `@Error` in the stdlib API (`@Throws(IoError)`), callers must handle | Result-returning APIs |

Option A keeps `@Throws` lists meaningful (domain/IO failures) and avoids annotating every
indexing call; aborts print the existing "no silent garbage" diagnostic. Option B is the
literal "everything is checked" reading and would make most functions carry `@Throws`.

### 4. Migration

1. Enforcement ships behind a project flag (`[language] checked_exceptions = true`) and
   warns first; the stdlib is migrated module by module (each extended-tier package needs a
   version bump and docs page, as usual).
2. `catch (e: String)` and `throw "text"` stay valid with the flag off, and are errors with
   it on, with a mechanical rewrite hint (`tinox migrate-exceptions`, same approach as
   `migrate-semicolons`).
3. `@ErrorMap` remains as the legacy way to map string codes; new code uses `@Error`'s
   status.

## Staging

1. **Done:** `@Error`/`@Throws` declarations validated; `@Error(..., status)` feeds the HTTP table.
2. Typed `throw` / type-dispatching `catch` (fixes the "all clauses run" behaviour too).
3. Checker for `@Throws` (rules above) + warnings, then errors behind the flag.
4. Stdlib migration and `tinox migrate-exceptions`.

Decisions needed from you: the table in §3 (A or B per row), and whether error classes may
extend each other (needed for "catch a supertype"; otherwise exact-class matching only).
