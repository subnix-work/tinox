//! Stdlib smoke gate: use every module in crates/tinox-core once.
//!
//! Motivation (2026-07-17): several stdlib modules called builtins that
//! exist neither in the runtime nor in codegen (httpGet, base64Decode,
//! uuidGenerate, …) — they never compiled, because no test ever imported
//! them. Since an import codegens the entire module, a minimal call per
//! module is enough to catch ghost builtins and codegen breakage across
//! the whole module (the IR verifier reports every unreferenced
//! declaration).
//!
//! The .tnx cases are generated at runtime (nothing checked in).
//! Known breakages live in KNOWN_BROKEN: a listed module MUST fail
//! (otherwise "stale entry" → keep the list up to date), an unlisted one
//! MUST pass. A completeness test enforces that every new stdlib module
//! gets a smoke case here (or a justification in EXCLUDED).

mod common;
use common::{parse_case, run_case};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Modules that don't compile/run today — each entry is an open bug
/// (see bugs.md Bug 20, grouped there by error class). When fixed:
/// remove the entry, otherwise the test fails with "stale entry".
///
/// Empty since 2026-07-18: all tinox-core modules compile and run
/// (Bug 20 fully resolved, Bugs 21-32). Add newly broken modules here
/// with a justification + bugs.md reference.
const KNOWN_BROKEN: &[&str] = &[];

/// Modules without a smoke case, with a justification.
const EXCLUDED: &[(&str, &str)] = &[
    ("db", "needs [database] config; covered by the orm_sqlite_* e2e cases"),
    (
        "http3_server",
        "needs TINOX_HTTP3=1 (opt-in, default OFF -- unlike OpenSSL, ngtcp2/nghttp3 aren't universally installed); a smoke case without this flag would fail at link time. Covered by crates/tinox/tests/http3_server_curl.rs (its own process, real curl --http3-only, skips cleanly instead of failing when ngtcp2/nghttp3/HTTP3-curl are missing on the build machine).",
    ),
];

struct Smoke {
    /// File stem in crates/tinox-core (= test name stdlib_smoke_<key>)
    key: &'static str,
    /// Import lines (usually just the module itself)
    imports: &'static [&'static str],
    /// Statements in the main body
    body: &'static str,
    /// Expected stdout lines (exact, ordered)
    expects: &'static [&'static str],
    /// Alternative/additional: substrings the output must contain
    contains: &'static [&'static str],
}

const SMOKES: &[Smoke] = &[
    Smoke {
        key: "array",
        imports: &["tinox.core.array"],
        body: "println(Arrays::findMin([3, 1, 2]));",
        expects: &["1"],
        contains: &[],
    },
    Smoke {
        key: "asm",
        imports: &["tinox.core.asm"],
        body: "let a: Assembler = Assembler::new();\n    Assembler::emit(a, 144);\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "base64",
        imports: &["tinox.core.base64"],
        body: r#"println(Base64::encode("hi"));"#,
        expects: &["aGk="],
        contains: &[],
    },
    Smoke {
        key: "bitmap",
        imports: &["tinox.core.bitmap"],
        body: "let b: Bitmap = Bitmap::create(2, 2, 0);\n    Bitmap::setPixel(b, 0, 0, 5);\n    println(Bitmap::getPixel(b, 0, 0));",
        expects: &["5"],
        contains: &[],
    },
    Smoke {
        key: "cache",
        imports: &["tinox.core.cache"],
        body: "let c: Cache<String, Int64> = Cache::new(4);\n    Cache::set(c, \"a\", 1);\n    let o: Option<Int64> = Cache::get(c, \"a\");\n    println(o.unwrap());",
        expects: &["1"],
        contains: &[],
    },
    Smoke {
        key: "collections",
        imports: &["tinox.core.collections"],
        body: "let s: Stack<Int64> = Stack::new();\n    s.push(7);\n    println(s.pop());",
        expects: &["7"],
        contains: &[],
    },
    Smoke {
        key: "complex",
        imports: &["tinox.core.complex"],
        body: "println(Complex::magnitude(Complex::new(3.0, 4.0)).toString());",
        expects: &["5"],
        contains: &[],
    },
    Smoke {
        key: "compress",
        imports: &["tinox.core.compress"],
        body: "let packed: List<Int64> = Compress::gzip([104, 105]);\n    let back: List<Int64> = Compress::gunzip(packed);\n    println(Compress::lastGunzipOk());\n    println(back.len());",
        expects: &["true", "2"],
        contains: &[],
    },
    Smoke {
        key: "cron",
        imports: &["tinox.core.cron"],
        body: "let e: CronExpr = Cron::parse(\"* * * * *\");\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "crypto",
        imports: &["tinox.core.crypto"],
        body: r#"if Crypto::sha256("abc").len() > 0 { println("yes"); } else { println("no"); }"#,
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "csv",
        imports: &["tinox.core.csv"],
        body: "let rows: List<List<String>> = Csv::parse(\"a,b\\nc,d\");\n    println(rows.len());\n    println(rows[0][1]);",
        expects: &["2", "b"],
        contains: &[],
    },
    Smoke {
        key: "debug",
        imports: &["tinox.core.debug"],
        body: "Debug::assert(true, \"never\");\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "decimal",
        imports: &["tinox.core.decimal"],
        body: "println(Decimal::toString(Decimal::fromInt(3)));",
        expects: &["3"],
        contains: &[],
    },
    Smoke {
        key: "encoding",
        imports: &["tinox.core.encoding"],
        body: "println(Encoding::fromCharCode(65));",
        expects: &["A"],
        contains: &[],
    },
    Smoke {
        key: "env",
        imports: &["tinox.core.env"],
        body: "Env::setVar(\"TINOX_SMOKE\", \"x1\");\n    println(Env::getVar(\"TINOX_SMOKE\"));",
        expects: &["x1"],
        contains: &[],
    },
    Smoke {
        key: "events",
        imports: &["tinox.core.events", "tinox.core.json"],
        body: "let em: EventEmitter = EventEmitter::new();\n    EventEmitter::on(em, \"ping\", v => { println(\"pong\"); });\n    EventEmitter::emit(em, \"ping\", Json::parse(\"1\"));",
        expects: &["pong"],
        contains: &[],
    },
    Smoke {
        key: "fmt",
        imports: &["tinox.core.fmt"],
        body: "println(Fmt::sprintf(\"a%sb\", [\"X\"]));",
        expects: &["aXb"],
        contains: &[],
    },
    Smoke {
        key: "format",
        imports: &["tinox.core.format"],
        body: "println(Format::padLeft(\"7\", 3, \"0\"));",
        expects: &["007"],
        contains: &[],
    },
    Smoke {
        key: "fs",
        imports: &["tinox.core.fs"],
        body: "Fs::writeFile(\"smoke.txt\", \"hi\");\n    println(Fs::readFile(\"smoke.txt\"));",
        expects: &["hi"],
        contains: &[],
    },
    Smoke {
        key: "graph",
        imports: &["tinox.core.graph"],
        body: "let g: Graph<Int64> = Graph::new();\n    Graph::addNode(g, \"a\", 1);\n    Graph::addNode(g, \"b\", 2);\n    Graph::addEdge(g, \"a\", \"b\", 1.0);\n    println(Graph::neighbors(g, \"a\").len());",
        expects: &["1"],
        contains: &[],
    },
    Smoke {
        key: "hash",
        imports: &["tinox.core.hash"],
        body: r#"if Hash::hashString("a") == Hash::hashString("a") { println("yes"); } else { println("no"); }"#,
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "heap",
        imports: &["tinox.core.heap"],
        body: "let h: Heap<Int64> = Heap::new();\n    Heap::push(h, 3);\n    Heap::push(h, 1);\n    println(Heap::pop(h));",
        expects: &["1"],
        contains: &[],
    },
    Smoke {
        key: "hex",
        imports: &["tinox.core.hex"],
        body: r#"println(Hex::encode("A"));"#,
        expects: &["41"],
        contains: &[],
    },
    Smoke {
        key: "hpack",
        imports: &["tinox.core.hpack"],
        body: "let h: HpackHeader = HpackHeader::new(\"a\", \"b\");\n    println(h.name);",
        expects: &["a"],
        contains: &[],
    },
    Smoke {
        key: "http",
        imports: &["tinox.core.http"],
        body: "Http::setHeader(\"x-a\", \"b\");\n    Http::clearHeaders();\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "http2_server",
        imports: &["tinox.core.http2_server"],
        body: "println(Http2FrameType::HEADERS());",
        expects: &["1"],
        contains: &[],
    },
    Smoke {
        key: "http_server",
        imports: &["tinox.core.http_server"],
        body: "let s: HttpServer = HttpServer::new(0);\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "ini",
        imports: &["tinox.core.ini"],
        body: "let c: IniConfig = Ini::parse(\"[s]\\nk=v\");\n    println(IniConfig::getString(c, \"s\", \"k\", \"?\"));",
        expects: &["v"],
        contains: &[],
    },
    Smoke {
        key: "io",
        imports: &["tinox.core.io"],
        body: r#"println(Paths::join("a", "b.txt"));"#,
        expects: &["a/b.txt"],
        contains: &[],
    },
    Smoke {
        key: "iter",
        imports: &["tinox.core.iter"],
        body: "let xs: List<Int64> = Iter::repeat(7, 3);\n    println(xs.len());",
        expects: &["3"],
        contains: &[],
    },
    Smoke {
        key: "json",
        imports: &["tinox.core.json"],
        body: "let v: JsonValue = Json::parse(\"{\\\"a\\\": 5}\");\n    println(Json::getField(v, \"a\").getInt());",
        expects: &["5"],
        contains: &[],
    },
    Smoke {
        key: "jwt",
        imports: &["tinox.core.jwt", "tinox.core.json"],
        body: "var p: Map<String, JsonValue> = Map::new();\n    let t: String = Jwt::encode(p, \"secret\");\n    if t.len() > 0 { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "kubernetes",
        // No network I/O here on purpose (CI has no live cluster to talk
        // to) -- this only needs to catch ghost-builtin/codegen breakage
        // in the module itself, the same job every other SMOKES case
        // does. Real CRUD/Watch behavior is verified manually against a
        // live minikube cluster (see the module's own commit history).
        imports: &["tinox.core.kubernetes", "tinox.core.json"],
        body: "var containers: List<Container> = [];\n    containers.push(Container::simple(\"c\", \"nginx:alpine\"));\n    let pod: Pod = Pod::create(\"smoke-pod\", \"default\", containers);\n    let j: String = Json::serialize(pod);\n    if j.contains(\"nginx:alpine\") { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "logger",
        imports: &["tinox.core.logger"],
        body: "let l: Logger = Logger::new(\"t\");\n    Logger::info(l, \"hello-smoke\");",
        expects: &[],
        contains: &["hello-smoke"],
    },
    Smoke {
        key: "mathf",
        imports: &["tinox.core.mathf"],
        body: "println(Mathf::sqrt(9.0).toString());",
        expects: &["3"],
        contains: &[],
    },
    Smoke {
        key: "math",
        imports: &["tinox.core.math"],
        body: "println(Math::abs(-5));",
        expects: &["5"],
        contains: &[],
    },
    Smoke {
        key: "mathx",
        imports: &["tinox.core.mathx"],
        body: "println(Mathx::gcd(12, 8));",
        expects: &["4"],
        contains: &[],
    },
    Smoke {
        key: "metrics",
        imports: &["tinox.core.metrics"],
        body: "MetricsRegistry::incCounter(\"smoke\");\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "msgpack",
        imports: &["tinox.core.msgpack"],
        body: "let v: MsgpackValue = MsgpackValue { kind: \"int\", stringValue: \"\", intValue: 42, floatValue: 0.0, boolValue: false, arrayValue: [], objectValue: Map::new() };\n    let bytes: List<Int64> = Msgpack::encode(v);\n    println(Msgpack::decode(bytes).getInt());",
        expects: &["42"],
        contains: &[],
    },
    Smoke {
        key: "oauth2",
        imports: &["tinox.core.oauth2"],
        body: "let c: OAuth2Client = OAuth2Client::new(\"https://example.com/authorize\", \"https://example.com/token\", \"cid\", \"csecret\", \"https://app.example.com/cb\");\n    let r: OAuth2AuthorizeRequest = c.buildAuthorizeUrl(\"openid\");\n    if r.url.contains(\"code_challenge=\") { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "oidc",
        imports: &["tinox.core.oidc"],
        body: "let c: OidcClient = OidcClient::new(\"https://issuer.example.com\", \"https://example.com/authorize\", \"https://example.com/token\", \"https://example.com/jwks.json\", \"cid\", \"csecret\", \"https://app.example.com/cb\");\n    let r: OAuth2AuthorizeRequest = c.buildAuthorizeUrl(\"openid email\");\n    if r.url.contains(\"code_challenge=\") { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "option",
        imports: &["tinox.core.option"],
        body: "let o: Option<Int64> = Option::some(5);\n    println(o.unwrap());",
        expects: &["5"],
        contains: &[],
    },
    Smoke {
        key: "pool",
        imports: &["tinox.core.pool"],
        // Exercises the factory-callback path (fnc field on a generic class):
        // acquire() calls pool.factory() when nothing is pooled.
        body: "let p: Pool<Int64> = Pool::newWithFactory(2, fnc() -> Int64 { return 7; });\n    println(Pool::acquire(p).toString());",
        expects: &["7"],
        contains: &[],
    },
    Smoke {
        key: "process",
        imports: &["tinox.core.process"],
        body: "if Process::pid() > 0 { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "queue",
        imports: &["tinox.core.queue"],
        body: "let q: PriorityQueue<String> = PriorityQueue::new();\n    PriorityQueue::enqueue(q, \"a\", 1);\n    println(PriorityQueue::dequeue(q));",
        expects: &["a"],
        contains: &[],
    },
    Smoke {
        key: "random",
        imports: &["tinox.core.random"],
        body: "let x: Int64 = Random::nextInt(6);\n    if x >= 0 { println(\"ok\"); } else { println(\"ok\"); }",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "ratelimit",
        imports: &["tinox.core.ratelimit"],
        body: "let r: RateLimiter = RateLimiter::new(2, 1000);\n    if RateLimiter::allow(r) { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "redis",
        imports: &["tinox.core.redis"],
        body: "let client: RedisClient = RedisClient::connect(\"127.0.0.1\", 1);\n    println(client.conn <= 0);",
        expects: &["true"],
        contains: &[],
    },
    Smoke {
        key: "regex",
        imports: &["tinox.core.regex"],
        body: r#"if Regex::isMatch("ab+", "abb") { println("yes"); } else { println("no"); }"#,
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "rest.server",
        imports: &["tinox.core.rest.server"],
        body: "let g: GET = GET::new(\"/x\");\n    println(g.path);",
        expects: &["/x"],
        contains: &[],
    },
    Smoke {
        key: "rest.client",
        imports: &["tinox.core.rest.client"],
        body: "let c: RestClient = RestClient::new(\"http://localhost:1\");\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "result",
        imports: &["tinox.core.result"],
        body: "let r: Result<Int64> = Result::ok(5);\n    println(r.unwrap());",
        expects: &["5"],
        contains: &[],
    },
    Smoke {
        key: "semaphore",
        imports: &["tinox.core.semaphore"],
        body: "let s: Semaphore = Semaphore::new(1);\n    if Semaphore::tryAcquire(s) { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "set",
        imports: &["tinox.core.set"],
        body: "let s: Set<Int64> = Set::new();\n    Set::add(s, 1);\n    Set::add(s, 1);\n    println(Set::size(s));",
        expects: &["1"],
        contains: &[],
    },
    Smoke {
        key: "smtp",
        imports: &["tinox.core.smtp"],
        body: "let client: SmtpClient = SmtpClient::connect(\"127.0.0.1\", 1);\n    println(client.conn <= 0);",
        expects: &["true"],
        contains: &[],
    },
    Smoke {
        key: "socket",
        imports: &["tinox.core.socket"],
        body: "let s: Socket = Socket::createTcp();\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "sort",
        imports: &["tinox.core.sort"],
        body: "let xs: List<Int64> = Sort::quickSort([3, 1, 2]);\n    println(xs[0]);",
        expects: &["1"],
        contains: &[],
    },
    Smoke {
        key: "sse",
        imports: &["tinox.core.sse"],
        body: "let srv: Int64 = SseServer::listen(0);\n    println(srv > 0);",
        expects: &["true"],
        contains: &[],
    },
    Smoke {
        key: "string",
        imports: &["tinox.core.string"],
        body: r#"println(Strings::toUpperCase("ab"));"#,
        expects: &["AB"],
        contains: &[],
    },
    Smoke {
        key: "time",
        imports: &["tinox.core.time"],
        body: "let d: Duration = Time::newDuration(120);\n    println(d.getMinutes());",
        expects: &["2"],
        contains: &[],
    },
    Smoke {
        key: "toml",
        imports: &["tinox.core.toml"],
        body: "let v: TomlValue = Toml::parse(\"a = 1\");\n    if v.isTable() { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "tpl",
        imports: &["tinox.core.tpl", "tinox.core.json"],
        body: "var m: Map<String, JsonValue> = Map::new();\n    println(Template::render(\"hi\", m));",
        expects: &["hi"],
        contains: &[],
    },
    Smoke {
        key: "trie",
        imports: &["tinox.core.trie"],
        body: "let t: Trie = Trie::new();\n    Trie::insert(t, \"ab\");\n    if Trie::search(t, \"ab\") { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "ui",
        // No WebSocket/HTTP I/O here on purpose (matches kubernetes' own
        // "no live cluster in CI" reasoning) -- this only needs to catch
        // ghost-builtin/codegen breakage in the module itself. Real
        // client/server protocol behavior is covered by
        // crates/tinox/tests/tinox_ui_*.rs (real compiled examples, live
        // WebSocket round-trips).
        imports: &["tinox.core.ui", "tinox.core.json"],
        body: "let c: Component = Component::label(\"hi\");\n    println(c.type);\n    let j: String = Json::serialize(c);\n    if j.contains(\"\\\"type\\\":\\\"Label\\\"\") { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["Label", "yes"],
        contains: &[],
    },
    Smoke {
        key: "uri",
        imports: &["tinox.core.uri"],
        body: r#"println(Uri::encode("a b"));"#,
        expects: &["a%20b"],
        contains: &[],
    },
    Smoke {
        key: "uuid",
        imports: &["tinox.core.uuid"],
        body: "println(Uuid::generate().len());",
        expects: &["36"],
        contains: &[],
    },
    Smoke {
        key: "amqp091",
        imports: &["tinox.core.amqp091"],
        // No real broker in the smoke gate — 127.0.0.1 on a port with no
        // listener gives a fast "connection refused" (no DNS, no latency),
        // but still covers the module's full codegen path (dial ->
        // socketCreateTcp/socketConnect/httpConnFromFd).
        body: "println(Amqp091::dial(\"127.0.0.1\", 39217));",
        expects: &["-1"],
        contains: &[],
    },
    Smoke {
        key: "amqp10",
        imports: &["tinox.core.amqp10"],
        // Same as amqp091 above: no real broker in the smoke gate, but
        // still covers the module's codegen path.
        body: "println(Amqp10::dial(\"127.0.0.1\", 39220));",
        expects: &["-1"],
        contains: &[],
    },
    Smoke {
        key: "websocket",
        imports: &["tinox.core.websocket"],
        // Pure codec logic without a socket (the full path including the
        // handshake is covered by tests/e2e/ws_handshake_frames.tnx).
        body: "let f: WsFrame = WsFrame { fin: true, opcode: 1, payload: Ws::textToBytes(\"hi\"), rsv1: false };\n    println(Ws::text(f));",
        expects: &["hi"],
        contains: &[],
    },
    Smoke {
        key: "validation",
        imports: &["tinox.core.validation"],
        body: r#"if Validation::isNumeric("123") { println("yes"); } else { println("no"); }"#,
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "xml",
        imports: &["tinox.core.xml"],
        body: "let n: XmlNode = Xml::parse(\"<a>x</a>\");\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
    Smoke {
        key: "yaml",
        imports: &["tinox.core.yaml"],
        body: "let v: YamlValue = Yaml::parse(\"a: 1\");\n    if v.isMap() { println(\"yes\"); } else { println(\"no\"); }",
        expects: &["yes"],
        contains: &[],
    },
    Smoke {
        key: "zip",
        imports: &["tinox.core.zip"],
        body: "let a: ZipArchive = Zip::create(\"smoke.zip\");\n    println(\"ok\");",
        expects: &["ok"],
        contains: &[],
    },
];

/// Issue #185: crates/tinox-core is now one shared `tinox/core/` tree at
/// the crate root (every core-tier module resolves through `stdlib_dir()`
/// unconditionally, with no per-module scoping the way extended-tier
/// dependency dirs have) — repointed straight at that tree so every
/// existing per-module scan below it (`array/`, `base64/`, ...) is found
/// exactly like before the migration, no `scan_module_dir` changes needed
/// for this tier.
fn stdlib_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tinox-core/tinox/core")
}

/// The extended-tier stdlib split off crates/tinox-core into its own
/// directory (see CLAUDE.md's core/extended stdlib split notes) — still
/// walked here for `stdlib_smoke_completeness`'s inventory, alongside
/// `stdlib_dir()`, so extended modules keep needing a smoke case too, even
/// though they no longer resolve via `stdlib_dir()`/`TINOX_PATH` at build
/// time. Each SMOKES case for an extended module gets an auto-synthesized
/// tinox.toml plus a `tinox install` run by `common::run_case`, exactly
/// like an e2e case importing an extended module.
fn ext_stdlib_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tinox-core-ext")
}

fn emit_case(s: &Smoke) -> (String, String) {
    let name = format!("stdlib_smoke_{}", s.key);
    let mut src = String::new();
    for e in s.expects {
        src.push_str(&format!("// expect: {e}\n"));
    }
    for c in s.contains {
        src.push_str(&format!("// expect-contains: {c}\n"));
    }
    src.push('\n');
    for imp in s.imports {
        src.push_str(&format!("import {imp};\n"));
    }
    // Issue #149 stage 3: no top-level `fn` allowed anymore -- wrap in the
    // fixed `class Main { fnc main() -> Int32 }` entry-point shape
    // codegen's `emit_class_main_entry_point` requires.
    src.push_str(&format!(
        "\nclass Main {{\n    fnc main() -> Int32 {{\n    {}\n    return 0;\n    }}\n}}\n",
        s.body
    ));
    (name, src)
}

/// A module is either a legacy single `<name>.tnx` file (not yet migrated),
/// a `<name>/` directory of one-type-per-file `.tnx` files directly inside
/// it (migrated, one-type-per-file convention), or — since the compiler
/// gained nested `tinox.core.X.Y` stdlib import support (rest/client,
/// rest/server) — a `<name>/` directory containing ONLY subdirectories
/// (no `.tnx` files of its own), a pure grouping directory: descend one
/// level and key each child as `"<name>.<child>"`, matching the import
/// path (`tinox.core.rest.client`) rather than the directory name.
fn scan_module_dir(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{} readable: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .flat_map(|p| -> Vec<String> {
            if p.is_dir() {
                let name = p.file_name().map(|n| n.to_string_lossy().to_string());
                let Some(name) = name else { return vec![] };
                // Issue #185: extended-tier modules now nest their real
                // content one more level deeper, under their own
                // module-name-scoped `tinox/core/<name>/` prefix
                // (crates/tinox-core-ext/<name>/tinox/core/<name>/...,
                // matching what a published/downloaded package already
                // looks like on disk) — transparently unwrap it here so
                // the module's top-level identity (`name`) keeps keying
                // the smoke-test inventory exactly like before the
                // migration. Core-tier (`stdlib_dir()`, repointed straight
                // at its own shared `tinox/core/` tree above) never hits
                // this branch, since it has no such per-module prefix.
                let nested = p.join("tinox").join("core").join(&name);
                let p = if nested.is_dir() { nested } else { p };
                let has_own_tnx = fs::read_dir(&p)
                    .map(|entries| {
                        entries
                            .filter_map(|e| e.ok())
                            .any(|e| e.path().extension().map(|x| x == "tnx").unwrap_or(false))
                    })
                    .unwrap_or(true); // unreadable -> don't misclassify as a grouping dir
                if has_own_tnx {
                    vec![name]
                } else {
                    fs::read_dir(&p)
                        .map(|entries| {
                            entries
                                .filter_map(|e| e.ok())
                                .filter(|e| e.path().is_dir())
                                .filter_map(|e| e.file_name().to_str().map(|s| format!("{name}.{s}")))
                                .collect()
                        })
                        .unwrap_or_default()
                }
            } else if p.extension().map(|x| x == "tnx").unwrap_or(false) {
                p.file_stem()
                    .map(|s| vec![s.to_string_lossy().to_string()])
                    .unwrap_or_default()
            } else {
                vec![]
            }
        })
        .collect()
}

/// Every stdlib module has a smoke case or a justification in EXCLUDED.
#[test]
fn stdlib_smoke_completeness() {
    // Core-tier (stdlib_dir(), resolves unconditionally) + extended-tier
    // (ext_stdlib_dir(), needs a declared+installed dependency) — both
    // trees need every module covered, see CLAUDE.md's core/extended
    // stdlib split notes and ext_stdlib_dir()'s own doc comment.
    let mut modules: BTreeSet<String> = scan_module_dir(&stdlib_dir()).into_iter().collect();
    modules.extend(scan_module_dir(&ext_stdlib_dir()));
    let covered: BTreeSet<String> = SMOKES.iter().map(|s| s.key.to_string()).collect();
    let excluded: BTreeSet<String> = EXCLUDED.iter().map(|(k, _)| k.to_string()).collect();

    let missing: Vec<&String> = modules
        .iter()
        .filter(|m| !covered.contains(*m) && !excluded.contains(*m))
        .collect();
    assert!(
        missing.is_empty(),
        "stdlib modules without a smoke case (add an entry in SMOKES or a justification in EXCLUDED): {missing:?}"
    );

    let stale: Vec<&String> = covered
        .iter()
        .chain(excluded.iter())
        .filter(|k| !modules.contains(*k))
        .collect();
    assert!(
        stale.is_empty(),
        "SMOKES/EXCLUDED entries without a module file (deleted module? remove the entry): {stale:?}"
    );

    for k in KNOWN_BROKEN {
        assert!(
            covered.contains(*k),
            "KNOWN_BROKEN entry {k:?} has no smoke case"
        );
    }
}

fn generate_all(shard: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tinox-stdlib-smoke-{}-{shard}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir smoke dir");
    for s in SMOKES {
        let (name, src) = emit_case(s);
        // Issue #149 stage 3: `class Main` (see emit_case) requires the
        // file to be named exactly `Main.tnx` (one-class-per-file rule) --
        // each case gets its own subdirectory, same convention e2e.rs uses
        // for directory-based cases.
        let case_dir = dir.join(&name);
        fs::create_dir_all(&case_dir).expect("mkdir case dir");
        fs::write(case_dir.join("Main.tnx"), src).expect("write case");
    }
    dir
}

fn run_shard(shard: usize, num_shards: usize) {
    let dir = generate_all(shard);
    let mut unexpected_failures = Vec::new();
    let mut stale_entries = Vec::new();
    for (i, s) in SMOKES.iter().enumerate() {
        if i % num_shards != shard {
            continue;
        }
        let name = format!("stdlib_smoke_{}", s.key);
        // `parse_case` sets `case.name` from the file stem ("Main") --
        // override back to the unique per-case name, since `run_case`
        // (crates/tinox/tests/common/mod.rs) derives the isolated workdir
        // path AND the output binary name from `case.name`; leaving it as
        // "Main" for every case would collide across the concurrently-run
        // cases sharing one process id.
        let mut case = parse_case(&dir.join(&name).join("Main.tnx"));
        case.name = name.clone();
        let known_bad = KNOWN_BROKEN.contains(&s.key);
        match run_case(&case) {
            Ok(()) if known_bad => stale_entries.push(s.key.to_string()),
            Ok(()) => {}
            Err(_) if known_bad => {}
            Err(msg) => unexpected_failures.push(format!("== {name} ==\n{msg}")),
        }
    }

    let mut problems = Vec::new();
    if !unexpected_failures.is_empty() {
        problems.push(format!(
            "{} stdlib smoke cases fail (module broken → fix it or add to KNOWN_BROKEN + bugs.md):\n\n{}",
            unexpected_failures.len(),
            unexpected_failures.join("\n\n")
        ));
    }
    if !stale_entries.is_empty() {
        problems.push(format!(
            "stale KNOWN_BROKEN (now passing — remove the entry): {}",
            stale_entries.join(", ")
        ));
    }
    assert!(problems.is_empty(), "{}", problems.join("\n\n"));
}

#[test]
fn stdlib_smoke_shard_0() { run_shard(0, 4); }
#[test]
fn stdlib_smoke_shard_1() { run_shard(1, 4); }
#[test]
fn stdlib_smoke_shard_2() { run_shard(2, 4); }
#[test]
fn stdlib_smoke_shard_3() { run_shard(3, 4); }
