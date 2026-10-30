//! multipart/form-data parsing (issue #542), driven by curl's own multipart encoder (an
//! independent implementation): binary values with NUL survive byte for byte, and malformed,
//! truncated or over-budget bodies are rejected with 400 and a reason.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const CTRL: &str = r#"import tinox.core.bytes
import tinox.core.http_server

class Ctrl
{
    @POST("/upload")
    fnc upload(@HttpContext ctx: HttpContext) -> HttpContext
    {
        const r = Multipart::parse(ctx.request.getHeader("Content-Type"), Bytes::fromRequestBody(ctx.request.body), MultipartLimits { maxParts: 3, maxFieldBytes: 64, maxFileBytes: 3500000, maxHeaderBytes: 512 })
        if (!r.ok) {
            ctx.response.badRequest("error: " + r.error)
            return ctx
        }
        var out = "parts=" + r.parts.len().toString()
        for p in r.parts {
            out = out + "|" + p.name + ":" + p.safeFileName() + ":" + p.data.len().toString() + ":" + p.contentType
            if (p.data.len() > 0) {
                out = out + ":first=" + p.data.get(0).toString() + ":last=" + p.data.get(p.data.len() - 1).toString()
            }
        }
        ctx.response.text(out)
        return ctx
    }
}
"#;

fn curl(port: u16, args: &[&str]) -> (u16, String) {
    for attempt in 0..10 {
        let out = Command::new("curl")
            .args(["-s", "-w", "\n%{http_code}", "-X", "POST"])
            .args(args)
            .arg(format!("http://127.0.0.1:{port}/upload"))
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if let Some((body, code)) = text.rsplit_once('\n') {
            if let Ok(code) = code.trim().parse::<u16>() {
                if code != 0 {
                    return (code, body.to_string());
                }
            }
        }
        if attempt < 9 {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    panic!("no answer from the server");
}

#[test]
fn binary_values_round_trip_and_bad_bodies_are_rejected() {
    if Command::new("curl").arg("--version").output().is_err() {
        eprintln!("SKIP multipart (curl not installed)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tinox-multipart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/http_server/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"mp\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/http_server").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Ctrl.tnx"), CTRL).unwrap();
    std::fs::write(dir.join("Main.tnx"), "import Ctrl\nclass Main {\n    fnc main() -> Int32 {\n        return 0\n    }\n}\n").unwrap();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let exe = dir.join("mp");
    let build = Command::new(tinox).args(["build", "Main.tnx", "-o"]).arg(&exe).env("TINOX_PORT", port.to_string()).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    let server = Command::new(&exe).current_dir(&dir).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let _guard = Kill(server);
    std::thread::sleep(Duration::from_millis(600));

    // Binary file: NUL, CR, LF, a fake boundary line, and every byte value.
    let mut bin: Vec<u8> = vec![0, 13, 10, 0];
    bin.extend_from_slice(b"\r\n--not-the-boundary\r\n");
    bin.extend(0..=255u8);
    bin.push(0);
    let bin_path = dir.join("blob.bin");
    std::fs::write(&bin_path, &bin).unwrap();
    let empty_path = dir.join("empty.txt");
    std::fs::write(&empty_path, b"").unwrap();

    let blob = format!("blob=@{};type=application/octet-stream;filename=../../etc/pa\"ss.bin", bin_path.display());
    let (code, body) = curl(port, &["-F", "title=hello wörld", "-F", &blob, "-F", &format!("empty=@{}", empty_path.display())]);
    assert_eq!(code, 200, "{body}");
    assert!(body.starts_with("parts=3|"), "{body}");
    assert!(body.contains("|title::12:text/plain:first=104:last=100"), "{body}"); // "hello wörld" is 12 bytes
    // safeFileName() drops the path and keeps the (curl percent-encoded) quote; size, first and last byte are exact.
    assert!(body.contains(&format!("|blob:pa%22ss.bin:{}:application/octet-stream:first=0:last=0", bin.len())), "{body}");
    assert!(body.contains("|empty:empty.txt:0:"), "{body}");

    // A multi-megabyte file (under the server's 4 MB body cap) is parsed quickly and exactly.
    let big_path = dir.join("big.bin");
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&big_path, &big).unwrap();
    let started = std::time::Instant::now();
    let (code, body) = curl(port, &["-F", &format!("big=@{}", big_path.display())]);
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("|big:big.bin:3000000:") && body.contains(&format!(":first=0:last={}", big[2_999_999])), "{body}");
    assert!(started.elapsed() < Duration::from_secs(5), "slow: {:?}", started.elapsed());

    // Too many parts, oversized field, missing boundary, truncated body.
    let (code, body) = curl(port, &["-F", "a=1", "-F", "b=2", "-F", "c=3", "-F", "d=4"]);
    assert_eq!((code, body.as_str()), (400, "error: too many parts (limit 3)"));
    let (code, body) = curl(port, &["-F", &format!("big={}", "x".repeat(65))]);
    assert_eq!(code, 400);
    assert!(body.contains("part 'big' is 65 bytes, limit 64"), "{body}");
    let (code, body) = curl(port, &["-H", "Content-Type: multipart/form-data", "-d", "x"]);
    assert_eq!(code, 400);
    assert!(body.contains("missing/invalid boundary"), "{body}");
    let (code, body) = curl(port, &["-H", "Content-Type: multipart/form-data; boundary=zz", "--data-binary", "--zz\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\nvalue-without-end"]);
    assert_eq!(code, 400);
    assert!(body.contains("truncated body"), "{body}");
    let (code, body) = curl(port, &["-H", "Content-Type: multipart/form-data; boundary=zz", "--data-binary", "--zz\r\nContent-Type: text/plain\r\n\r\nv\r\n--zz--\r\n"]);
    assert_eq!(code, 400);
    assert!(body.contains("without a form-data name"), "{body}");
    // The server survived every rejection.
    let (code, _) = curl(port, &["-F", "ok=1"]);
    assert_eq!(code, 200);
    let _ = std::fs::remove_dir_all(&dir);
}
