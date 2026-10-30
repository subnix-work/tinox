//! Large uploads and upload ownership (issue #542): TINOX_MAX_BODY_BYTES raises the body cap (default 4 MiB -> 413),
//! `@MultipartParam` binds the parsed body, file parts are spooled to 0600 files that are streamed back in bounded
//! chunks and removed by `defer r.release()` after success AND after an exception; `Multipart::sweep` removes files a
//! dead process left behind. Driven by curl's multipart encoder, spool directory inspected from outside.

use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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
import tinox.core.env
import tinox.core.http_server

class Ctrl
{
    // Binding by annotation: the handler only sees a valid, parsed form.
    @POST("/form")
    fnc form(@MultipartParam form: MultipartResult, @HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.text("fields=" + form.parts.len().toString() + " title=" + form.get("title").text())
        return ctx
    }

    @POST("/big")
    fnc big(@HttpContext ctx: HttpContext) -> HttpContext
    {
        const r = Multipart::parse(ctx.request.getHeader("Content-Type"), Bytes::fromRequestBody(ctx.request.body), MultipartLimits { maxParts: 4, maxFieldBytes: 1024, maxFileBytes: 60000000, maxHeaderBytes: 512 })
        if (!r.ok) {
            ctx.response.badRequest(r.error)
            return ctx
        }
        defer r.release()
        r.spool(Env::getVar("SPOOL"))
        const part = r.get("file")
        var total: Int64 = 0
        const reader = part.open()
        var more = true
        while (more) {
            const chunk = reader.read(1048576)
            if (chunk.len() == 0) {
                more = false
            } else {
                total = total + chunk.len()
            }
        }
        reader.close()
        ctx.response.text("streamed " + total.toString() + " size=" + part.size.toString() + " inmemory=" + part.data.len().toString())
        return ctx
    }

    // Fails after spooling: defer must still remove the file.
    @POST("/boom")
    fnc boom(@HttpContext ctx: HttpContext) -> HttpContext
    {
        const r = Multipart::parse(ctx.request.getHeader("Content-Type"), Bytes::fromRequestBody(ctx.request.body), MultipartLimits { maxParts: 4, maxFieldBytes: 1024, maxFileBytes: 60000000, maxHeaderBytes: 512 })
        defer r.release()
        r.spool(Env::getVar("SPOOL"))
        throw "handler failed after spooling"
    }
}
"#;

fn curl(port: u16, path: &str, args: &[&str]) -> (u16, String) {
    for attempt in 0..10 {
        let out = Command::new("curl")
            .args(["-s", "-w", "\n%{http_code}", "-X", "POST"])
            .args(args)
            .arg(format!("http://127.0.0.1:{port}{path}"))
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

fn files_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect()
}

fn start(exe: &Path, dir: &Path, spool: &Path, max_body: Option<&str>) -> Kill {
    let mut cmd = Command::new(exe);
    cmd.current_dir(dir).env("SPOOL", spool).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(v) = max_body {
        cmd.env("TINOX_MAX_BODY_BYTES", v);
    }
    Kill(cmd.spawn().unwrap())
}

#[test]
fn large_uploads_are_spooled_streamed_and_cleaned_up() {
    if Command::new("curl").arg("--version").output().is_err() {
        eprintln!("SKIP multipart spool (curl not installed)");
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("tinox-mpspool-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let spool = dir.join("spool");
    std::fs::create_dir_all(&spool).unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/http_server/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"mpspool\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/http_server").display()
        ),
    )
    .unwrap();
    std::fs::write(dir.join("Ctrl.tnx"), CTRL).unwrap();
    std::fs::write(dir.join("Main.tnx"), "import Ctrl\nclass Main {\n    fnc main() -> Int32 {\n        return 0\n    }\n}\n").unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let install = Command::new(tinox).arg("install").current_dir(&dir).output().unwrap();
    assert!(install.status.success(), "{}", String::from_utf8_lossy(&install.stderr));
    let exe = dir.join("mpspool");
    let build = Command::new(tinox).args(["build", "Main.tnx", "-o"]).arg(&exe).env("TINOX_PORT", port.to_string()).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "{}{}", String::from_utf8_lossy(&build.stdout), String::from_utf8_lossy(&build.stderr));

    let big_path = dir.join("big.bin");
    let big: Vec<u8> = (0..30_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&big_path, &big).unwrap();
    let file_arg = format!("file=@{}", big_path.display());

    // Default cap (4 MiB): a 30 MB body is refused with 413 and nothing is written.
    {
        let _server = start(&exe, &dir, &spool, None);
        std::thread::sleep(Duration::from_millis(600));
        let (code, _) = curl(port, "/big", &["-F", &file_arg]);
        assert_eq!(code, 413);
        assert!(files_in(&spool).is_empty());
    }

    // Raised cap: the 30 MB upload is accepted, spooled, streamed back in chunks and the file is gone afterwards.
    let _server = start(&exe, &dir, &spool, Some("67108864"));
    std::thread::sleep(Duration::from_millis(600));
    let (code, body) = curl(port, "/form", &["-F", "title=hello"]);
    assert_eq!((code, body.as_str()), (200, "fields=1 title=hello"), "@MultipartParam binding");
    let (code, body) = curl(port, "/form", &["-H", "Content-Type: multipart/form-data", "-d", "x"]);
    assert_eq!(code, 400, "a bad form never reaches the handler: {body}");
    assert!(body.contains("boundary"), "{body}");

    let (code, body) = curl(port, "/big", &["-F", &file_arg]);
    assert_eq!(code, 200, "{body}");
    assert_eq!(body, "streamed 30000000 size=30000000 inmemory=0", "the in-memory copy is dropped after spooling");
    assert!(files_in(&spool).is_empty(), "release() must remove the spooled file: {:?}", files_in(&spool));

    // Exception after spooling: `defer r.release()` still cleans up.
    let (code, _) = curl(port, "/boom", &["-F", &file_arg]);
    assert_eq!(code, 500);
    assert!(files_in(&spool).is_empty(), "{:?}", files_in(&spool));

    // Rejected (over the handler's own budget): nothing was spooled either.
    let (code, _) = curl(port, "/big", &["-F", "a=1", "-F", "b=2", "-F", "c=3", "-F", "d=4", "-F", "e=5"]);
    assert_eq!(code, 400);
    assert!(files_in(&spool).is_empty());

    // Files left by a process that died (never released) are found by the sweeper only when old enough, and the
    // files themselves are private (0600).
    let stale = spool.join("tinox-upload-stale.part");
    std::fs::write(&stale, b"left behind").unwrap();
    std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o600)).unwrap();
    let other = spool.join("keep-me.txt");
    std::fs::write(&other, b"not ours").unwrap();
    let sweeper = dir.join("sweep");
    std::fs::create_dir_all(&sweeper).unwrap();
    std::fs::write(
        sweeper.join("Main.tnx"),
        format!("import tinox.core.bytes\n\nclass Main\n{{\n    fnc main() -> Int32 {{\n        println(MultipartResult::sweep(\"{0}\", 3600000))\n        sleep(1200)\n        println(MultipartResult::sweep(\"{0}\", 1000))\n        return 0\n    }}\n}}\n", spool.display()),
    )
    .unwrap();
    let swept = Command::new(tinox).args(["run", "Main.tnx"]).current_dir(&sweeper).output().unwrap();
    let out = String::from_utf8_lossy(&swept.stdout);
    assert!(swept.status.success(), "{out}{}", String::from_utf8_lossy(&swept.stderr));
    assert_eq!(out.lines().collect::<Vec<_>>(), ["0", "1"], "young files stay, old ones go: {out}");
    assert!(!stale.exists() && other.exists(), "only tinox-upload-*.part files are touched");
    let _ = std::fs::remove_dir_all(&dir);
}
