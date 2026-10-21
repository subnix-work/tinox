//! Explicit Linux compilation targets (issue #310).
//!
//! One `Target` value is the single source of truth for the LLVM target
//! triple, data layout and `llc -mtriple`; every compilation step (IR header,
//! `llc`) reads it from here instead of hardcoding x86-64.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    X86_64Linux,
    Aarch64Linux,
}

impl Target {
    /// The architecture this compiler binary itself runs on, if supported.
    pub fn host() -> Option<Target> {
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Some(Target::X86_64Linux)
        } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
            Some(Target::Aarch64Linux)
        } else {
            None
        }
    }

    /// Parse a user-supplied target name (`TINOX_TARGET`). Accepts the short
    /// names and the canonical LLVM triples; anything else is a hard error.
    pub fn parse(s: &str) -> Result<Target, String> {
        match s {
            "x86_64-linux" | "x86_64-unknown-linux-gnu" | "linux-x86_64" | "x86-64" => {
                Ok(Target::X86_64Linux)
            }
            "aarch64-linux" | "aarch64-unknown-linux-gnu" | "linux-arm64" | "linux-aarch64"
            | "arm64-linux" => Ok(Target::Aarch64Linux),
            other => Err(format!(
                "unsupported target '{}' (supported: x86_64-linux, aarch64-linux)",
                other
            )),
        }
    }

    pub fn triple(self) -> &'static str {
        match self {
            Target::X86_64Linux => "x86_64-unknown-linux-gnu",
            Target::Aarch64Linux => "aarch64-unknown-linux-gnu",
        }
    }

    pub fn datalayout(self) -> &'static str {
        match self {
            Target::X86_64Linux => "e-m:e-i64:64-f80:128-n8:16:32:64-S128",
            Target::Aarch64Linux => "e-m:e-i8:8:32-i16:16:32-i64:64-i128:128-n32:64-S128",
        }
    }

    /// Value for `llc -march=`.
    pub fn llc_march(self) -> &'static str {
        match self {
            Target::X86_64Linux => "x86-64",
            Target::Aarch64Linux => "aarch64",
        }
    }
}

impl Default for Target {
    /// Host architecture; falls back to x86-64 (the historical default) on
    /// hosts outside the supported set.
    fn default() -> Self {
        Target::host().unwrap_or(Target::X86_64Linux)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names_and_triples() {
        assert_eq!(Target::parse("aarch64-linux"), Ok(Target::Aarch64Linux));
        assert_eq!(Target::parse("x86_64-unknown-linux-gnu"), Ok(Target::X86_64Linux));
        assert!(Target::parse("riscv64-linux").is_err());
        assert!(Target::parse("aarch64-apple-darwin").is_err());
    }

    #[test]
    fn triple_and_march_consistent() {
        assert!(Target::Aarch64Linux.triple().starts_with("aarch64"));
        assert_eq!(Target::Aarch64Linux.llc_march(), "aarch64");
        assert_eq!(Target::X86_64Linux.llc_march(), "x86-64");
    }
}
