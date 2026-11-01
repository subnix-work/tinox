//! Issue #561: tinox.core.unicode against an independent implementation. Python's `str.upper()`,
//! `str.lower()`, `chr()` and UTF-8 codec decide what every scalar value must map to and how
//! malformed input must be classified; the compiled module has to agree for ALL 1,112,064 values.

use std::path::PathBuf;
use std::process::Command;

const MAIN: &str = r#"import tinox.core.unicode
import tinox.core.encoding

class Main
{
    fnc hex(codes: List<Int64>) -> String
    {
        var out: String = ""
        for c in codes { out = out + c.toString() + " " }
        return out
    }

    fnc main() -> Int32
    {
        var cp: Int64 = 1
        var roundtrip_failures: Int64 = 0
        while (cp <= 1114111)
        {
            if (cp < 55296 || cp > 57343)
            {
                const one: String = Unicode::fromCodePoints([cp])
                const back: List<Int64> = Unicode::codePoints(one)
                if (back.len() != 1 || back[0] != cp || one.len() != Unicode::byteOffset(one, 1)) { roundtrip_failures = roundtrip_failures + 1 }
                const up: String = Unicode::toUpper(one)
                const lo: String = Unicode::toLower(one)
                if (up != one) { println("U " + cp.toString() + " " + Main::hex(Unicode::codePoints(up))) }
                if (lo != one) { println("L " + cp.toString() + " " + Main::hex(Unicode::codePoints(lo))) }
            }
            cp = cp + 1
        }
        println("roundtrip_failures=" + roundtrip_failures.toString())

        // multi-character input, edge mappings and units
        println("mix=" + Unicode::toUpper("straße ŉ ǆ ﬃ ΐ"))
        println("lower=" + Unicode::toLower("İSTANBUL ΑΣ"))
        println("count=" + Unicode::codePointCount("é😀é").toString() + " bytes=" + "é😀é".len().toString())
        println("offset=" + Unicode::byteOffset("hé😀x", 3).toString() + " end=" + Unicode::byteOffset("hé😀x", 4).toString())

        // malformed UTF-8 is reported with its first bad byte, never replaced
        const cases: List<List<Int64>> = [[97, 255, 98], [195], [237, 160, 128], [192, 128], [244, 144, 128, 128], [226, 130], [97, 98, 128]]
        for bytes in cases
        {
            const bad: String = Encoding::fromUtf8(bytes)
            var message: String = "none"
            try { Unicode::codePointCount(bad) } catch (e: String) { message = e }
            println("bad " + Main::hex(bytes) + "valid=" + Unicode::isValid(bad).toString() + " " + message)
        }
        var scalar: String = "none"
        try { Unicode::fromCodePoints([97, 55296]) } catch (e: String) { scalar = e }
        println("surrogate=" + scalar)
        var nul: String = "none"
        try { Unicode::fromCodePoints([0]) } catch (e: String) { nul = e }
        println("nul=" + nul)
        var range: String = "none"
        try { Unicode::byteOffset("ab", 3) } catch (e: String) { range = e }
        println("range=" + range)
        return 0
    }
}
"#;

const EXPECTED: &str = r#"
import sys
out = []
for cp in range(1, 0x110000):
    if 0xD800 <= cp <= 0xDFFF:
        continue
    c = chr(cp)
    for tag, m in (("U", c.upper()), ("L", c.lower())):
        if m != c:
            out.append("%s %d %s" % (tag, cp, "".join("%d " % ord(x) for x in m)))
out.append("roundtrip_failures=0")
out.append("mix=" + "straße ŉ ǆ ﬃ ΐ".upper())
# Python applies the final-sigma rule; the module deliberately does not, so compare with "ΑΣ" -> "ασ"
out.append("lower=" + "İSTANBUL ΑΣ".replace("Σ", "σ").lower())
out.append("count=%d bytes=%d" % (len("é😀é"), len("é😀é".encode())))
out.append("offset=%d end=%d" % (len("hé😀".encode()), len("hé😀x".encode())))
for raw in ([97, 255, 98], [195], [237, 160, 128], [192, 128], [244, 144, 128, 128], [226, 130], [97, 98, 128]):
    try:
        bytes(raw).decode("utf-8")
        out.append("unexpectedly valid")
    except UnicodeDecodeError as e:
        out.append("bad %svalid=false UNICODE_ERROR: invalid UTF-8 at byte %d" % ("".join("%d " % b for b in raw), e.start))
out.append("surrogate=UNICODE_ERROR: element 1 is not a Unicode scalar value")
out.append("nul=UNICODE_ERROR: element 0 is not a Unicode scalar value")
out.append("range=UNICODE_ERROR: code point index 3 is out of range")
sys.stdout.write("\n".join(out) + "\n")
"#;

#[test]
fn case_mapping_code_points_and_malformed_input_agree_with_python() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("SKIP: python3 not installed");
        return;
    }
    let expected = Command::new("python3").args(["-I", "-c", EXPECTED]).output().unwrap();
    assert!(expected.status.success(), "{}", String::from_utf8_lossy(&expected.stderr));
    let expected = String::from_utf8(expected.stdout).unwrap();

    let dir: PathBuf = std::env::temp_dir().join(format!("tinox-unicode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tinox.toml"), format!("[package]\ncompiler = \"{}\"\nname = \"u\"\nversion = \"0.0.0\"\n", env!("CARGO_PKG_VERSION"))).unwrap();
    std::fs::write(dir.join("Main.tnx"), MAIN).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{}\n{}", &stdout[stdout.len().saturating_sub(2000)..], String::from_utf8_lossy(&out.stderr));
    let got: Vec<&str> = stdout.lines().collect();
    let want: Vec<&str> = expected.lines().collect();
    for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
        assert_eq!(g, w, "first difference at output line {i}");
    }
    assert_eq!(got.len(), want.len(), "line count differs");
    let _ = std::fs::remove_dir_all(&dir);
}
