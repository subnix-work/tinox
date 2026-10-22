# Runtime configuration

Build once, then run the same artifact with different deployment inputs:

```sh
tinox build Main.tnx -o service
TINOX_CONFIG_SERVICE_TOKEN=local-token TINOX_CONFIG_SERVICE_PORT=8081 ./service
TINOX_CONFIG_SERVICE_TOKEN=other-token TINOX_CONFIG_SERVICE_PORT=9091 ./service
```

`@Config("key", "required")` rejects missing and empty values.
`@Config("key", "optional")` supplies an empty string, zero or false when missing.
`@Config("key", "default", "value")` uses its fallback only when the key is missing.
All three validate supplied values, including defaults, before any application code,
DI initialization, startup banner or listener runs. Errors aggregate on stderr and
exit with status 78. Explicit empty input never falls back to a default. Empty
strings are accepted for optional/default String fields; empty numeric/Bool input
is invalid. Field initializers do not supply configuration defaults.

Precedence, highest first:

1. Environment variable `TINOX_CONFIG_` plus the normalized key.
2. The properties file selected by `TINOX_CONFIG_FILE`, or `application.properties`
   in the process working directory when no selector is supplied.
3. The annotation's declared default or optional missing-value behavior.

An explicit file replaces the default file. An absent default file is allowed;
an unreadable explicit file is an error even when environment values are supplied.
Properties use `key=value` lines; leading key/value spaces and trailing key
spaces are trimmed. Value trailing spaces are preserved. Blank lines and lines
starting with `#` or `!` after indentation are ignored. There are no escapes,
interpolation or continuation lines. The last occurrence of a duplicate key wins.
Malformed lines, embedded NULs and more than 256 distinct properties fail startup.
Configuration is read once at startup and is immutable for the process lifetime.

Keys contain ASCII letters, digits, dots, hyphens and underscores. Environment
normalization uppercases letters and replaces dots and hyphens with underscores:
`service.port` becomes `TINOX_CONFIG_SERVICE_PORT`. Declarations whose keys normalize
to the same variable fail startup. `TINOX_CONFIG_FILE` is reserved. Repeated
identical declarations share a resolved value; conflicting types, policies or
defaults for one key fail startup. Up to 256 distinct declarations are supported.

String, Bool, Int8/16/32/64 and UInt8/16/32/64 are supported. Integers are decimal,
with an optional sign, and must fit the field type. Unsigned fields reject a minus
sign. Overflow, whitespace, suffixes and hex notation are rejected. Bool accepts
exactly `true` or `false`, with case preserved. Generic configuration classes and inheritance of configuration fields are
not supported; inject a configuration component to share values. Configuration fields can be omitted from struct literals and are
injected into generated service and DI instances as well.

Use `@Sensitive` for secrets. Secret defaults and field initializers are forbidden
because literals would be compiled into the artifact. Generated `toString()` masks
these fields and generated JSON excludes them. Configuration diagnostics never
include values. Dev UI configuration introspection redacts **all** property and
annotation values, including undeclared properties and environment overrides.
Application code still controls its own output: do not print secret fields directly.

One-argument `@Config("key")` retains legacy missing values and permissive integer
and boolean getter behavior. Migrate to an explicit policy to enable strict typed
validation. These legacy declarations also receive runtime environment overrides.

`tinox.toml` remains build/toolchain configuration. Runtime application values are
not read by the compiler or included in generated IR, build cache keys or images.
Non-secret annotation defaults are intentionally compiled into the artifact.
Do not copy deployment properties or secret files into an image; mount them or
provide environment values when the service starts. No CLI arguments are consumed
for application configuration in this version.
