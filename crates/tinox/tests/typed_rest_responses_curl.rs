//! Exercise the same typed controller over TCP and QUIC with independent curl.
//! Stage checkout sources locally, without relying on published package versions.
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use serde_yaml::Value;

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Workspace(PathBuf);
impl Drop for Workspace {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn copy_tree(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), target.join(entry.file_name())).unwrap();
        }
    }
}

fn curl(work: &Path, url: &str, http3: bool, args: &[&str]) -> Option<(u16, String, String)> {
    let mut cmd = Command::new("curl");
    cmd.args(["--silent", "--show-error", "--max-time", "5", "--dump-header"])
        .arg(work.join("headers.txt"))
        .arg("--output").arg(work.join("body.json"))
        .args(["--write-out", "%{http_code}"]);
    if http3 { cmd.args(["--http3-only", "--insecure"]); }
    let out = cmd.args(args).arg(url).output().expect("curl must be installed");
    if !out.status.success() {
        eprintln!("curl {url}: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    Some((
        String::from_utf8(out.stdout).unwrap().parse().unwrap(),
        std::fs::read_to_string(work.join("headers.txt")).unwrap().to_lowercase(),
        std::fs::read_to_string(work.join("body.json")).unwrap(),
    ))
}

fn verify(http3: bool) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let work = Workspace(std::env::temp_dir().join(format!("tinox-typed-response-{}-{http3}", std::process::id())));
    let _ = std::fs::remove_dir_all(&work.0);
    std::fs::create_dir_all(&work.0).unwrap();
    for module in ["http_server", "http3_server", "json"] {
        copy_tree(&root.join("crates/tinox-core-ext").join(module).join("tinox"),
            &work.0.join(".tinox/deps/tinox.core").join(module).join("checkout/tinox"));
    }
    std::fs::write(work.0.join("tinox.toml"), "[package]\ncompiler = \"2.5.1\"\nname = \"typed_response\"\nversion = \"0.0.0\"\n").unwrap();
    std::fs::write(work.0.join("Main.tnx"), "import Ctrl\nclass Main { fnc main() -> Int32 { return 0 } }\n").unwrap();
    std::fs::write(work.0.join("User.tnx"), "import tinox.core.json\n@JsonSerializable class User { var name: String }\n").unwrap();
    let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
    let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = if http3 { udp.local_addr().unwrap().port() } else { tcp.local_addr().unwrap().port() };
    let annotation = if http3 { format!("@Http3RestController({port}, \"cert.pem\", \"key.pem\")") } else { String::new() };
    let source = if http3 { "import tinox.core.http3_server\n" } else { "" }.to_string() + r#"
import tinox.core.http_server
import User
class Ctrl {
    @GET @Path("/users/:id") @Produces("application/json") @StatusCode(201)
    fn get(@PathParam id: Int64) -> Response<User> {
        if (id == 1) { return Response<User>::ok(User { name: "Ada" }).header("X-Result", "success") }
        if (id == 2) { return Response<User>::noContent().header("X-Result", "empty") }
        if (id == 3) { return Response<User>::ok(User { name: "Grace" }).withStatus(202) }
        if (id == 4) { return Response<User>::error(200, "invalid status") }
        return Response<User>::error(404, "missing \"user\"\n\\path\x01").header("X-Result", "error")
    }
    @POST @Path("/users") @Consumes("application/json")
    fn create(@PostParam user: User) -> Response<User> {
        if (user.name == "") { return Response<User>::error(422, "name required") }
        return Response<User>::ok(user)
    }
    @GET @Path("/scalar") fn scalar() -> Response<Int32> { let n: Int32 = -7
    return Response<Int32>::ok(n) }
    @GET @Path("/string") fn stringValue() -> Response<String> { return Response<String>::ok("quoted \"text\"") }
    @GET @Path("/bool") fn boolValue() -> Response<Bool> { return Response<Bool>::ok(false) }
    @GET @Path("/int") fn intValue() -> Response<Int64> { return Response<Int64>::ok(9223372036854775807) }
    @GET @Path("/list") fn listValue() -> Response<List<User>> { return Response<List<User>>::ok([User { name: "Ada" }]) }
    @GET @Path("/raw") fn raw(@HttpContext ctx: HttpContext) -> HttpContext {
        ctx.response.status(202).text("manual")
        return ctx
    }
}
"#;
    // Imports precede annotations and class declarations.
    let source = source.replace("class Ctrl", &format!("{annotation}\nclass Ctrl"));
    std::fs::write(work.0.join("Ctrl.tnx"), source).unwrap();
    for (fixture, target) in [("selfsigned_cert.pem", "cert.pem"), ("selfsigned_key.pem", "key.pem")] {
        std::fs::copy(root.join("tests/fixtures/tls").join(fixture), work.0.join(target)).unwrap();
    }
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let contract = Command::new(tinox).args(["contract", "Main.tnx"]).current_dir(&work.0).output().unwrap();
    assert!(contract.status.success(), "{}", String::from_utf8_lossy(&contract.stderr));
    let contract: Value = serde_yaml::from_slice(&contract.stdout).unwrap();
    let responses = &contract["paths"]["/users/{id}"]["get"]["responses"];
    assert!(responses["204"]["content"].is_null());
    assert_eq!(responses["2XX"]["content"]["application/json"]["schema"]["$ref"].as_str(), Some("#/components/schemas/User"));
    assert_eq!(contract["components"]["schemas"]["User"]["properties"]["name"]["type"].as_str(), Some("string"));
    let exe = work.0.join("server");
    let build = Command::new(tinox).args(["build", "Main.tnx", "-o"]).arg(&exe)
        .env("TINOX_PORT", port.to_string()).env("TINOX_HTTP3", if http3 { "1" } else { "0" })
        .current_dir(&work.0).output().unwrap();
    assert!(build.status.success(), "{}", String::from_utf8_lossy(&build.stderr));
    drop(tcp);
    drop(udp);
    let base = format!("{}://127.0.0.1:{port}", if http3 { "https" } else { "http" });
    // Repeat startup/readiness and all wire outcomes to cover timing-sensitive
    // behavior. Build once, restart the real server on every repetition.
    for iteration in 0..15 {
        let mut server = Server(Command::new(&exe).current_dir(&work.0).stdout(Stdio::null()).stderr(Stdio::inherit()).spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some((status, _, _)) = curl(&work.0, &format!("{base}/scalar"), http3, &[]) {
                assert_eq!(status, 200);
                break;
            }
            assert!(server.0.try_wait().unwrap().is_none(), "server exited before readiness");
            assert!(Instant::now() < deadline, "server readiness timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
        for (id, status, marker) in [(1, 201, "success"), (2, 204, "empty"), (9, 404, "error")] {
            let (observed, headers, body) = curl(&work.0, &format!("{base}/users/{id}"), http3, &[]).expect("request failed");
            assert_eq!(observed, status);
            assert!(headers.contains(&format!("x-result: {marker}\r\n")), "{headers}");
            if status == 204 {
                assert!(body.is_empty());
                assert!(!headers.contains("content-type:"));
                assert!(!headers.contains("content-length:"));
            } else {
                assert!(headers.contains("content-type: application/json"));
                let value: Value = serde_yaml::from_str(&body).unwrap();
                if status == 201 {
                    assert_eq!(value["name"].as_str(), Some("Ada"));
                } else {
                    assert_eq!(value["error"].as_str(), Some("missing \"user\"\n\\path\u{1}"));
                    assert!(body.contains("\\u0001"));
                    assert_eq!(responses["4XX"]["content"]["application/json"]["schema"]["properties"]["error"]["type"].as_str(), Some("string"));
                }
            }
        }
        for (path, expected, body) in [
            ("/users/3", 202, r#"{"name":"Grace"}"#),
            ("/users/4", 500, r#"{"error":"invalid typed response"}"#),
            ("/scalar", 200, "-7"),
            ("/bool", 200, "false"),
            ("/int", 200, "9223372036854775807"),
            ("/string", 200, r#""quoted \"text\"""#),
            ("/list", 200, r#"[{"name":"Ada"}]"#),
            ("/raw", 202, "manual"),
        ] {
            let (status, _, observed) = curl(&work.0, &format!("{base}{path}"), http3, &[]).unwrap();
            assert_eq!(status, expected, "{path}");
            assert_eq!(observed, body, "{path}");
        }
        let content_type = match iteration % 3 {
            0 => "Content-Type: application/json",
            1 => "content-type: application/json",
            _ => "cOnTeNt-TyPe: application/json",
        };
        let (status, _, body) = curl(&work.0, &format!("{base}/users"), http3,
            &["--header", content_type, "--data", r#"{"name":""}"#]).unwrap();
        assert_eq!(status, 422);
        assert_eq!(body, r#"{"error":"name required"}"#);
        let (status, _, _) = curl(&work.0, &format!("{base}/users"), http3, &["--data", "bad"]).unwrap();
        assert_eq!(status, 415);
    }
}

#[test]
fn typed_responses_tcp() { verify(false); }

#[test]
fn typed_responses_http3() {
    let curl = Command::new("curl").arg("-V").output().expect("curl");
    let libraries = Command::new("pkg-config").args(["--exists", "libngtcp2", "libngtcp2_crypto_ossl", "libnghttp3"])
        .status().map(|s| s.success()).unwrap_or(false);
    if !libraries || !String::from_utf8_lossy(&curl.stdout).contains("HTTP3") {
        eprintln!("SKIP typed_responses_http3: HTTP/3 curl and ngtcp2/nghttp3 development libraries required");
        return;
    }
    verify(true);
}
