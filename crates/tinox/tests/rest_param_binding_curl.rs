//! Real, curl-based coverage for the REST parameter binding annotations
//! (@PathParam/@QueryParam/@PostParam/@HttpContext, see CLAUDE.md's "REST
//! Parameter Binding Annotations" section) -- specifically the paths the
//! *migrated* existing tests (http3_annotated_curl.rs,
//! http_server_gc_stress_curl.rs) don't exercise: a String @PathParam, a
//! Bool @QueryParam, and -- the actual novel risk surface this feature
//! introduced -- the 400-on-missing/invalid-parameter guard path (backed
//! by the new strict `tinox_parse_*_checked` runtime helpers, which exist
//! specifically because the existing `tinox_string_to_int`/`_to_float`
//! silently return 0/0.0 on garbage input). Drives a real compiled server
//! with the system's own curl, not a simulated client, per this project's
//! "verify against a real, independent implementation" philosophy.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

/// See http_server_gc_stress_curl.rs's own copy of this helper for why a
/// hardcoded version string here would be a latent bug.
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

#[test]
fn rest_param_binding_end_to_end() {
    let tinox = env!("CARGO_BIN_EXE_tinox");
    let workdir = std::env::temp_dir().join(format!(
        "tinox-rest-param-binding-curl-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(&workdir).expect("mkdir workdir");

    std::fs::write(
        workdir.join("Greeting.tnx"),
        r#"import tinox.core.json;

@JsonSerializable
class Greeting
{
    var message: String;
    var loud: Bool;
}
"#,
    )
    .expect("write Greeting.tnx");

    std::fs::write(
        workdir.join("Item.tnx"),
        r#"import tinox.core.json;

@JsonSerializable
class Item
{
    var id: Int64;
    var name: String;
}
"#,
    )
    .expect("write Item.tnx");

    std::fs::write(
        workdir.join("Ctrl.tnx"),
        r#"import tinox.core.http_server;
import tinox.core.json;
import Greeting;
import Item;

class Ctrl
{
    // String @PathParam + Bool @QueryParam, auto-serialize class return.
    @GET
    @Path("/greet/:name")
    fn greet(@PathParam name: String, @QueryParam loud: Bool) -> Greeting
    {
        if (loud)
        {
            return Greeting { message: "HELLO, " + name + "!", loud: true };
        }
        return Greeting { message: "Hello, " + name, loud: false };
    }

    // No bound params at all -- auto-serialize List<class> return.
    @GET
    @Path("/items")
    fn items() -> List<Item>
    {
        return [Item { id: 1, name: "widget" }, Item { id: 2, name: "gadget" }];
    }

    // @PostParam + auto-serialize class return.
    @POST
    @Path("/items")
    @StatusCode(201)
    fn createItem(@PostParam item: Item) -> Item
    {
        return item;
    }

    // Int64 @PathParam + @HttpContext, manual mode (dynamic 200 vs 404 --
    // the case a fixed @StatusCode can't express).
    @GET
    @Path("/check/:id")
    fn check(@PathParam id: Int64, @HttpContext ctx: HttpContext) -> HttpContext
    {
        if (id == 42)
        {
            ctx.response.status(200).json("{\"found\":true}");
            return ctx;
        }
        ctx.response.status(404).json("{\"found\":false}");
        return ctx;
    }
}
"#,
    )
    .expect("write Ctrl.tnx");

    std::fs::write(
        workdir.join("Main.tnx"),
        r#"import Ctrl;

class Main
{
    fnc main() -> Int32
    {
        return 0;
    }
}
"#,
    )
    .expect("write Main.tnx");

    std::fs::write(
        workdir.join("tinox.toml"),
        format!(
            "[package]\nname = \"param_binding_server\"\nversion = \"0.0.0\"\ndescription = \"\"\n\n\
             [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"http_server\"\nversion = \"{}\"\n\n\
             [[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"json\"\nversion = \"{}\"\n",
            extended_module_version("http_server"),
            extended_module_version("json"),
        ),
    )
    .expect("write tinox.toml");
    let install = Command::new(tinox)
        .arg("install")
        .current_dir(&workdir)
        .output()
        .expect("spawn install");
    assert!(
        install.status.success(),
        "tinox install failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );

    let exe = workdir.join("param_binding_server");
    // tinox_HttpServer_listen's plain-TCP path binds a fixed port UNLESS
    // TINOX_PORT is set at *build* time (emit_route_code reads it via
    // std::env::var during codegen, not at runtime) -- http_server_gc_
    // stress_curl.rs already owns the default 8080 (see CLAUDE.md's port-
    // collision note: two test files claiming the same port makes them
    // flaky against each other whenever cargo runs the two test binaries
    // concurrently), so this explicitly claims its own port instead of
    // relying on the default.
    let build = Command::new(tinox)
        .arg("build")
        .arg(workdir.join("Main.tnx"))
        .arg("-o")
        .arg(&exe)
        .env("TINOX_PORT", "18099")
        .current_dir(&workdir)
        .output()
        .expect("spawn build");
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
        .expect("spawn server");
    let _guard = KillOnDrop(child);
    std::thread::sleep(Duration::from_millis(500));

    let base = "http://127.0.0.1:18099";
    // Retries on an empty/`000` result (curl's own signal for "couldn't
    // connect") rather than a single fixed sleep+fire: this test's fixed
    // port can transiently collide with load from the OTHER real-server-
    // spawning integration tests when `make check` runs everything in
    // parallel (found live: this test alone was 5/5 green in isolation,
    // but failed once as part of a full `make check` run with an empty
    // body where a real one was expected -- a transient connection
    // hiccup under contention, not a feature bug). Doesn't weaken the
    // actual assertions below, which still check exact values.
    let curl_body = |url: &str| -> String {
        for attempt in 0..5 {
            let out = Command::new("curl").args(["-s", url]).output().expect("spawn curl");
            let body = String::from_utf8_lossy(&out.stdout).to_string();
            if !body.is_empty() || attempt == 4 {
                return body;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        unreachable!()
    };
    let curl_status = |args: &[&str]| -> String {
        for attempt in 0..5 {
            let out = Command::new("curl")
                .args([&["-s", "-o", "/dev/null", "-w", "%{http_code}"], args].concat())
                .output()
                .expect("spawn curl");
            let status = String::from_utf8_lossy(&out.stdout).to_string();
            if (!status.is_empty() && status != "000") || attempt == 4 {
                return status;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        unreachable!()
    };
    let curl_body_with_args = |args: &[&str]| -> String {
        for attempt in 0..5 {
            let out = Command::new("curl").args([&["-s"], args].concat()).output().expect("spawn curl");
            let body = String::from_utf8_lossy(&out.stdout).to_string();
            if !body.is_empty() || attempt == 4 {
                return body;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        unreachable!()
    };

    // String @PathParam + Bool @QueryParam, auto-serialize.
    assert_eq!(
        curl_body(&format!("{base}/greet/Ada?loud=true")),
        r#"{"message":"HELLO, Ada!","loud":true}"#
    );
    assert_eq!(
        curl_body(&format!("{base}/greet/Ada?loud=false")),
        r#"{"message":"Hello, Ada","loud":false}"#
    );

    // Missing required @QueryParam -> 400, handler never called.
    assert_eq!(curl_status(&[&format!("{base}/greet/Ada")]), "400");
    // @QueryParam present but not a valid Bool -> 400.
    assert_eq!(curl_status(&[&format!("{base}/greet/Ada?loud=maybe")]), "400");

    // No bound params, auto-serialize List<class>.
    assert_eq!(
        curl_body(&format!("{base}/items")),
        r#"[{"id":1,"name":"widget"},{"id":2,"name":"gadget"}]"#
    );

    // @PostParam + auto-serialize, real StatusCode(201).
    let create_status = curl_status(&[
        "-X", "POST",
        "-H", "Content-Type: application/json",
        "-d", r#"{"id":0,"name":"sprocket"}"#,
        &format!("{base}/items"),
    ]);
    assert_eq!(create_status, "201");
    let create_body = curl_body_with_args(&[
        "-X", "POST",
        "-H", "Content-Type: application/json",
        "-d", r#"{"id":0,"name":"sprocket"}"#,
        &format!("{base}/items"),
    ]);
    assert_eq!(create_body, r#"{"id":0,"name":"sprocket"}"#);

    // Int64 @PathParam + @HttpContext manual mode, dynamic status.
    assert_eq!(curl_body(&format!("{base}/check/42")), r#"{"found":true}"#);
    assert_eq!(curl_status(&[&format!("{base}/check/42")]), "200");
    assert_eq!(curl_body(&format!("{base}/check/7")), r#"{"found":false}"#);
    assert_eq!(curl_status(&[&format!("{base}/check/7")]), "404");

    // Non-numeric Int64 @PathParam -> 400, handler never called (would
    // otherwise silently parse as 0 via the old tinox_string_to_int).
    assert_eq!(curl_status(&[&format!("{base}/check/not-a-number")]), "400");

    let _ = std::fs::remove_dir_all(&workdir);
}
