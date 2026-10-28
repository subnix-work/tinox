//! Raw wire validation is checked with curl before JSON binding or controller calls.
use serde_json::{json, Value};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Workspace(PathBuf);
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn copy(source: &Path, target: &Path) {
    std::fs::create_dir_all(target).unwrap();
    for e in std::fs::read_dir(source).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            copy(&e.path(), &target.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), target.join(e.file_name())).unwrap();
        }
    }
}
fn request(url: &str, body: Option<&str>) -> (u16, Value) {
    let mut c = Command::new("curl");
    c.args(["-sS", "--max-time", "3", "-w", "\n%{http_code}"]);
    if url.starts_with("https:") {
        c.args(["--http3-only", "--insecure"]);
    }
    if let Some(body) = body {
        c.args([
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            body,
        ]);
    }
    let out = c.arg(url).output().unwrap();
    assert!(
        out.status.success(),
        "request {body:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let (body, status) = text.rsplit_once('\n').unwrap();
    (
        status.parse().unwrap(),
        serde_json::from_str(body).expect(body),
    )
}
fn verify(http3: bool) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let work = Workspace(std::env::temp_dir().join(format!(
        "tinox-dto-validation-{}-{http3}",
        std::process::id()
    )));
    let _ = std::fs::remove_dir_all(&work.0);
    std::fs::create_dir_all(&work.0).unwrap();
    for module in ["http_server", "http3_server", "json"] {
        copy(
            &root
                .join("crates/tinox-core-ext")
                .join(module)
                .join("tinox"),
            &work
                .0
                .join(".tinox/deps/tinox.core")
                .join(module)
                .join("checkout/tinox"),
        );
    }
    for(name,source)in[
 ("tinox.toml","[package]\ncompiler=\"2.8.0\"\nname=\"dto_validation\"\nversion=\"0.0.0\"\n"),
 ("Main.tnx","import Ctrl\nclass Main { fnc main() -> Int32 { return 0 } }"),
 ("Base.tnx","import tinox.core.json\n@JsonSerializable class Base { @Required var id: Int64\n @Length(1,1) var name: String }"),
 ("Address.tnx","import tinox.core.json\n@JsonSerializable class Address { @Length(1,4) var city: String }"),
 ("FloatValue.tnx","import tinox.core.json\n@JsonSerializable class FloatValue { var value: Float32 }"),
 ("Node.tnx","import tinox.core.json\n@JsonSerializable class Node { @Optional var child: Node }"),
 ("Input.tnx","import tinox.core.json\nimport Base\nimport Address\nimport Node\n@JsonSerializable class Input extends Base { @Length(1,3) var name: String\n@Range(1,9) var age: Int32\n@Required var address: Address\n@Length(1,2) var addresses: List<Address>\n@Optional @Length(1,4) var note: String\n@Optional var node: Node\n @Optional var labels: Map<String,String>\n @Optional var bytes: List<UInt8>\n @Optional var ratio: Float32\n @Optional @Range(9007199254740992,9007199254740993) var big: Int64 }") ,
 ("Ctrl.tnx",r#"import tinox.core.http_server
import Input
import FloatValue
@ApplicationComponent class Ctrl {
 var calls: Int64 = 0
 @GET @Path("/float-literal") fn literal() -> FloatValue { return FloatValue { value: 1.5 as Float32 } }
 @GET @Path("/calls") fn count() -> Int64 { return this.calls }
 @POST @Path("/dto") fn accept(@PostParam input: Input) -> Input { this.calls = this.calls + 1
 if (input.ratio != 1.5) { throw "bad ratio" }
 return input }
}"#),]{std::fs::write(work.0.join(name),source).unwrap();}
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let port = if http3 {
        UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    } else {
        port
    };
    if http3 {
        let ctrl=std::fs::read_to_string(work.0.join("Ctrl.tnx")).unwrap().replace("@ApplicationComponent class Ctrl",&format!("@Http3RestController({port}, \"cert.pem\", \"key.pem\") @ApplicationComponent class Ctrl"));
        std::fs::write(
            work.0.join("Ctrl.tnx"),
            format!("import tinox.core.http3_server\n{ctrl}"),
        )
        .unwrap();
        for (source, target) in [
            ("selfsigned_cert.pem", "cert.pem"),
            ("selfsigned_key.pem", "key.pem"),
        ] {
            std::fs::copy(
                root.join("tests/fixtures/tls").join(source),
                work.0.join(target),
            )
            .unwrap();
        }
    }
    let exe = work.0.join("server");
    let build = Command::new(tinox)
        .args(["build", "Main.tnx", "-o"])
        .arg(&exe)
        .env("TINOX_PORT", port.to_string())
        .env("TINOX_HTTP3", if http3 { "1" } else { "0" })
        .current_dir(&work.0)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let contract = Command::new(tinox)
        .args(["contract", "Main.tnx"])
        .current_dir(&work.0)
        .output()
        .unwrap();
    assert!(
        contract.status.success(),
        "{}",
        String::from_utf8_lossy(&contract.stderr)
    );
    let contract: serde_yaml::Value = serde_yaml::from_slice(&contract.stdout).unwrap();
    let input = &contract["components"]["schemas"]["tinox.request.Input"];
    assert!(input["required"]
        .as_sequence()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("id")));
    assert_eq!(input["properties"]["name"]["minLength"].as_f64(), Some(1.0));
    assert_eq!(
        input["properties"]["addresses"]["minItems"].as_f64(),
        Some(1.0)
    );
    assert_eq!(input["properties"]["age"]["maximum"].as_f64(), Some(9.0));
    let base = format!(
        "{}://127.0.0.1:{port}",
        if http3 { "https" } else { "http" }
    );
    let child = Command::new(&exe)
        .current_dir(&work.0)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut server = Server(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut ready = Command::new("curl");
        if http3 {
            ready.args(["--http3-only", "--insecure"]);
        }
        let out = ready
            .args(["-s", "--max-time", "1"])
            .arg(format!("{base}/calls"))
            .output()
            .unwrap();
        if out.status.success() {
            break;
        }
        assert!(server.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        request(&format!("{base}/float-literal"), None),
        (200, json!({"value":1.5}))
    );
    let valid = json!({"id":1,"name":"é🦀","age":3,"address":{"city":"Rome"},"addresses":[{"city":"Oslo"}],"labels":{"env":"test"},"bytes":[0,255],"ratio":1.5,"big":9007199254740993_i64});
    let mut cases = vec![];
    for (field, replacement, path, code) in [
        ("id", None, "$.id", "required"),
        ("id", Some(Value::Null), "$.id", "non_null"),
        ("name", Some(json!("")), "$.name", "length"),
        ("age", Some(json!("3")), "$.age", "type"),
        ("age", Some(json!(0)), "$.age", "range"),
        ("age", Some(json!(2147483648_i64)), "$.age", "range"),
        (
            "address",
            Some(json!({"city":""})),
            "$.address.city",
            "length",
        ),
        (
            "addresses",
            Some(json!([{"city":"Rome"},{"city":""}])),
            "$.addresses[1].city",
            "length",
        ),
        (
            "addresses",
            Some(json!([null])),
            "$.addresses[0]",
            "non_null",
        ),
        ("note", Some(json!("")), "$.note", "length"),
        (
            "labels",
            Some(json!({"bad":3})),
            "$.labels[\"bad\"]",
            "type",
        ),
        ("bytes", Some(json!([256])), "$.bytes[0]", "range"),
        ("bytes", Some(json!([-1])), "$.bytes[0]", "range"),
        ("ratio", Some(json!(3.5e38)), "$.ratio", "range"),
        ("big", Some(json!(9007199254740994_i64)), "$.big", "range"),
    ] {
        let mut value = valid.clone();
        if let Some(v) = replacement {
            value[field] = v;
        } else {
            value.as_object_mut().unwrap().remove(field);
        }
        cases.push((value.to_string(), path.to_string(), code.to_string()));
    }
    for raw in [
        "",
        "{",
        "{\"id\":1,}",
        "{\"id\":\"abc\\",
        "null",
        "[]",
        "{\"id\":1} trailing",
        "{\"id\":NaN}",
        "{\"id\":9223372036854775808}",
        "{\"id\":1e999}",
        "{\"id\":01}",
        "{\"id\":1,\"id\":2}",
    ] {
        let code = if raw == "null" {
            "non_null"
        } else if raw == "[]" {
            "type"
        } else if raw.contains("922337") || raw.contains("1e999") {
            "number"
        } else if raw.contains("\"id\":2") {
            "duplicate"
        } else {
            "malformed"
        };
        cases.push((raw.into(), "$".into(), code.into()));
    }
    let mut deep = json!({});
    for _ in 0..40 {
        deep = json!({"child":deep});
    }
    let mut value = valid.clone();
    value["node"] = deep;
    cases.push((value.to_string(), "$".into(), "depth".into()));
    for _ in 0..15 {
        for (body, path, code) in &cases {
            let (status, error) = request(&format!("{base}/dto"), Some(body));
            assert_eq!(status, 400, "{body}: {error}");
            assert_eq!(
                error,
                json!({"error":"validation_failed","details":[{"path":path,"code":code}]}),
                "{body}"
            );
        }
    }
    let body_file = work.0.join("raw-nul.json");
    let mut raw = valid.to_string().into_bytes();
    raw.extend_from_slice(b"\0trailing bytes");
    std::fs::write(&body_file, raw).unwrap();
    let (status, error) = request(
        &format!("{base}/dto"),
        Some(&format!("@{}", body_file.display())),
    );
    assert_eq!(status, 400);
    assert_eq!(error["details"][0]["code"], "malformed");
    assert_eq!(
        request(&format!("{base}/calls"), None),
        (200, json!(0)),
        "invalid requests reached handler"
    );
    for note in [None, Some(Value::Null), Some(json!("good"))] {
        let mut body = valid.clone();
        if let Some(note) = note {
            body["note"] = note;
        }
        let (status, returned) = request(&format!("{base}/dto"), Some(&body.to_string()));
        assert_eq!(status, 200, "{returned}");
        assert_eq!(returned["name"], "é🦀");
        assert_eq!(returned["address"]["city"], "Rome");
        assert_eq!(returned["labels"], valid["labels"]);
        assert_eq!(returned["bytes"], valid["bytes"]);
        assert_eq!(returned["ratio"], valid["ratio"]);
        assert_eq!(returned["big"], valid["big"]);
    }
    // Escaped scalar pairs must bind to the same UTF-8 string validated for length.
    let escaped = valid.to_string().replace("é🦀", r"\u00e9\ud83e\udd80");
    let (status, body) = request(&format!("{base}/dto"), Some(&escaped));
    assert_eq!(status, 200);
    assert_eq!(body["name"], "é🦀");
    assert_eq!(request(&format!("{base}/calls"), None), (200, json!(4)));
}

#[test]
fn strict_parser_boundaries() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let executable =
        std::env::temp_dir().join(format!("tinox-request-parser-{}", std::process::id()));
    let out = Command::new("cc")
        .args(["-DTINOX_NO_GC", "-ffunction-sections", "-Wl,--gc-sections"])
        .arg(root.join("tests/support/request_validation.c"))
        .args(["-lm", "-lpthread", "-lz", "-o"])
        .arg(&executable)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new(&executable).output().unwrap();
    let _ = std::fs::remove_file(&executable);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn recursive_dto_validation_before_handler() {
    verify(false);
}
#[test]
fn recursive_dto_validation_http3() {
    let curl = Command::new("curl").arg("-V").output().unwrap();
    let libraries = Command::new("pkg-config")
        .args([
            "--exists",
            "libngtcp2",
            "libngtcp2_crypto_ossl",
            "libnghttp3",
        ])
        .status()
        .is_ok_and(|s| s.success());
    if !libraries || !String::from_utf8_lossy(&curl.stdout).contains("HTTP3") {
        eprintln!("SKIP DTO HTTP3: HTTP3 curl and development libraries required");
        return;
    }
    verify(true);
}
