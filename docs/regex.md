# Regular expression contracts

`tinox.core.regex` uses POSIX extended regular expressions, with the existing
translation of `\d`, `\D`, `\s`, `\S`, `\w` and `\W` character classes.
POSIX locale rules apply. Inline modifiers such as `(?i)` are not supported.

All five public operations validate their pattern and throw a catchable String
error for invalid syntax: `isMatch`, `findFirst`, `findAll`, `replaceAll` and
`split`. `Regex::isValid(pattern)` returns a Boolean instead. Existing calls
remain case-sensitive. Each operation has an additive `WithOptions` variant
with a final `ignoreCase: Bool` argument; validation uses the same flags as
execution. `isValidWithOptions` accepts that flag too.

```tinox
import tinox.core.regex

class Main
{
    fnc main() -> Int32
    {
        println(Regex::isMatchWithOptions("hello", "HELLO", true))
        try
        {
            Regex::isMatch("(", "text")
        }
        catch (error: String)
        {
            println(error)
        }
        return 0
    }
}
```

`isMatch` returns false for a valid non-match. `findFirst` returns an empty
String for both no match and an empty match, preserving its existing contract.
`findAll` returns non-overlapping matches. Replacement text is literal, with
no capture-group expansion. `split` retains the legacy match-list behavior;
it does not tokenize the subject around delimiters. Empty-subject and
zero-length-match iteration retain the existing operation-specific behavior.

Validation and execution each compile the pattern, so a public operation pays
for two compilations. Compiled-pattern caching and execution deadlines are not
provided. Syntax errors are catchable; native engine/resource failures other
than an ordinary non-match remain visible hard runtime failures. Legacy raw
`regex*` builtins retain their hard-failure contract for invalid patterns.

The change is verified by public catch-path and raw-failure fixtures, a
case-sensitive/case-insensitive matrix, and an independent POSIX C oracle.
Published package-version documentation remains immutable; these contracts
describe the workspace implementation pending its next package release.
