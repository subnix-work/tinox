//! Issue #311: binary-safe HTTP/1.1 bodies through `tinox.core.bytes`.
//! A compiled annotation-driven server echoes a request body (with a fixed
//! 0x00 0xff suffix) and serves a 256-value blob; the SYSTEM'S OWN curl is the
//! independent client and every received byte is compared against the
//! expected octets, including embedded NULs (a String-based path would
//! truncate at the first 0x00).

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root")
}

fn extended_module_version(module: &str) -> String {
    let manifest = repo_root().join("crates/tinox-core-ext").join(module).join("tinox.toml");
    std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.trim().strip_prefix("version"))
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .map(|rest| rest.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "1.0.0".to_string())
}

struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

const PORT: u16 = 18311;

#[test]
fn binary_bodies_round_trip_over_http() {
    if std::net::TcpListener::bind(("0.0.0.0", PORT)).is_err() {
        eprintln!("SKIP binary_bodies_round_trip_over_http (port {PORT} in use)");
        return;
    }
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = std::env::temp_dir().join(format!("tinox-http-bytes-curl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).unwrap();

    std::fs::write(
        workdir.join("BlobController.tnx"),
        r#"import tinox.core.http_server
import tinox.core.bytes

class BlobController
{
    @POST("/echo")
    fnc echo(@HttpContext ctx: HttpContext) -> HttpContext
    {
        const got: Bytes = Bytes::fromRequestBody(ctx.request.body)
        const tail: Bytes = Bytes::new(2)
        tail.set(1, 255)
        ctx.response.status(200).header("Content-Type", "application/octet-stream")
        ctx.response.body = got.concat(tail).asResponseBody()
        return ctx
    }

    @GET("/blob")
    fnc blob(@HttpContext ctx: HttpContext) -> HttpContext
    {
        const b: Bytes = Bytes::new(256)
        var i: Int64 = 0
        while i < 256
        {
            b.set(i, i)
            i = i + 1
        }
        ctx.response.status(200).header("Content-Type", "application/octet-stream")
        ctx.response.body = b.asResponseBody()
        return ctx
    }

    @GET("/text")
    fnc text(@HttpContext ctx: HttpContext) -> HttpContext
    {
        ctx.response.status(200).text("plain")
        return ctx
    }
}
"#,
    )
    .unwrap();
    std::fs::write(
        workdir.join("Main.tnx"),
        "import BlobController\n\nclass Main\n{\n    fnc main() -> Int32\n    {\n        return 0\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"http_bytes\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{}\"\n",
            extended_module_version("http_server"),
        ),
    )
    .unwrap();
    let install = Command::new(tinox).arg("install").current_dir(&workdir).output().unwrap();
    assert!(install.status.success(), "install failed: {}", String::from_utf8_lossy(&install.stderr));

    let exe = workdir.join("http_bytes_server");
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .env("TINOX_PORT", PORT.to_string())
        .current_dir(&workdir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let child = Command::new(&exe)
        .current_dir(&workdir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = KillOnDrop(child);
    std::thread::sleep(Duration::from_millis(500));

    let base = format!("http://127.0.0.1:{PORT}");
    let curl = |args: &[&str]| -> Vec<u8> {
        for attempt in 0..5 {
            let out = Command::new("curl").args(["-s"]).args(args).output().unwrap();
            if !out.stdout.is_empty() || attempt == 4 {
                return out.stdout;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        unreachable!()
    };

    let blob = curl(&[&format!("{base}/blob")]);
    let expected: Vec<u8> = (0..=255u8).collect();
    assert_eq!(blob, expected, "GET /blob must deliver all 256 byte values");

    // Request payloads with embedded NULs, high bytes and a larger size.
    let mut payloads: Vec<Vec<u8>> = vec![
        vec![0, 0, 0],
        vec![1, 0, 2, 0, 255, 0],
        (0..=255u8).rev().collect(),
    ];
    payloads.push((0..200_000u32).map(|i| (i.wrapping_mul(7) % 256) as u8).collect());
    for (n, p) in payloads.iter().enumerate() {
        let path = workdir.join(format!("req{n}.bin"));
        std::fs::write(&path, p).unwrap();
        let got = curl(&[
            "-X",
            "POST",
            "--data-binary",
            &format!("@{}", path.display()),
            "-H",
            "Content-Type: application/octet-stream",
            &format!("{base}/echo"),
        ]);
        let mut want = p.clone();
        want.extend_from_slice(&[0, 255]);
        assert_eq!(got.len(), want.len(), "payload {n}: length mismatch");
        assert!(got == want, "payload {n}: bytes differ");
    }

    // Content-Length on the wire is the byte length, not a strlen.
    let head = Command::new("curl")
        .args(["-s", "-D", "-", "-o", "/dev/null", &format!("{base}/blob")])
        .output()
        .unwrap();
    let headers = String::from_utf8_lossy(&head.stdout).to_lowercase();
    assert!(headers.contains("content-length: 256"), "headers: {headers}");

    // String path is unaffected after a bytes response on the same worker.
    assert_eq!(curl(&[&format!("{base}/text")]), b"plain");
    let _ = std::fs::remove_dir_all(&workdir);
}
