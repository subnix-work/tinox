//! Supported Linux compilation targets (issue #310).
//!
//! The target is chosen with `TINOX_TARGET` (`x86_64` or `aarch64`, plus the
//! usual aliases) and defaults to the host architecture. Only native builds are
//! supported: there is no sysroot/cross-toolchain contract yet, so requesting an
//! architecture other than the host's is a hard error instead of producing an
//! artifact that cannot be linked or run.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    X86_64,
    Aarch64,
}

impl Target {
    /// The architecture this compiler binary was built for, if supported.
    pub fn host() -> Result<Target, String> {
        match std::env::consts::ARCH {
            "x86_64" => Ok(Target::X86_64),
            "aarch64" => Ok(Target::Aarch64),
            other => Err(format!(
                "unsupported host architecture '{other}' (supported: x86_64, aarch64)"
            )),
        }
    }

    pub fn parse(name: &str) -> Result<Target, String> {
        match name.trim().to_ascii_lowercase().as_str() {
            "x86_64" | "x86-64" | "amd64" | "x86_64-unknown-linux-gnu" | "x86_64-linux-gnu" => {
                Ok(Target::X86_64)
            }
            "aarch64" | "arm64" | "aarch64-unknown-linux-gnu" | "aarch64-linux-gnu" => {
                Ok(Target::Aarch64)
            }
            other => Err(format!(
                "unsupported TINOX_TARGET '{other}' (supported: x86_64, aarch64)"
            )),
        }
    }

    /// Resolve and validate the requested target: `TINOX_TARGET` or the host.
    /// Fails for unknown names and for non-native targets.
    pub fn resolve() -> Result<Target, String> {
        let host = Target::host()?;
        let requested = match std::env::var("TINOX_TARGET") {
            Ok(v) if !v.trim().is_empty() => Target::parse(&v)?,
            _ => return Ok(host),
        };
        if requested != host {
            return Err(format!(
                "TINOX_TARGET={} requested on a {} host: cross-compilation is not supported yet \
                 (no sysroot/toolchain contract); build natively on a {} machine",
                requested.name(),
                host.name(),
                requested.name()
            ));
        }
        Ok(requested)
    }

    pub fn name(self) -> &'static str {
        match self {
            Target::X86_64 => "x86_64",
            Target::Aarch64 => "aarch64",
        }
    }

    pub fn triple(self) -> &'static str {
        match self {
            Target::X86_64 => "x86_64-unknown-linux-gnu",
            Target::Aarch64 => "aarch64-unknown-linux-gnu",
        }
    }

    pub fn data_layout(self) -> &'static str {
        match self {
            Target::X86_64 => "e-m:e-i64:64-f80:128-n8:16:32:64-S128",
            Target::Aarch64 => "e-m:e-i8:8:32-i16:16:32-i64:64-i128:128-n32:64-S128",
        }
    }

    /// Value for `llc -march=`.
    pub fn llc_arch(self) -> &'static str {
        match self {
            Target::X86_64 => "x86-64",
            Target::Aarch64 => "aarch64",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aliases() {
        assert_eq!(Target::parse("arm64"), Ok(Target::Aarch64));
        assert_eq!(Target::parse("AMD64"), Ok(Target::X86_64));
        assert_eq!(Target::parse("aarch64-unknown-linux-gnu"), Ok(Target::Aarch64));
        assert!(Target::parse("riscv64").is_err());
    }

    #[test]
    fn triples_and_arch_are_consistent() {
        for t in [Target::X86_64, Target::Aarch64] {
            assert!(t.triple().starts_with(t.name()));
            assert!(t.data_layout().starts_with("e-"));
        }
        assert_eq!(Target::Aarch64.llc_arch(), "aarch64");
        assert_eq!(Target::X86_64.llc_arch(), "x86-64");
    }
}
