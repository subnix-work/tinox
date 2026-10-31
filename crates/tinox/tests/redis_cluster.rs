//! Issue #545: Redis Cluster routing, MOVED/ASK handling and failure contract against three
//! real cluster nodes (redis/valkey processes, no replicas).
#![cfg(unix)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Proc(Child);
impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// A client port whose cluster-bus port (port + 10000) is free as well.
fn free_cluster_port() -> u16 {
    loop {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        if port < 50000 && TcpListener::bind(("127.0.0.1", port + 10000)).is_ok() {
            return port;
        }
    }
}

fn wait_port(port: u16) {
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("port {port} never opened");
}

fn cli(port: u16, args: &[&str]) -> String {
    let out = Command::new("redis-cli").args(["-p", &port.to_string()]).args(args).output().expect("redis-cli");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

const MAIN: &str = r#"import tinox.core.redis

class Main
{
    fnc indexOf(list: List<String>, value: String) -> Int64 {
        var i: Int64 = 0
        while i < list.len() {
            if (list[i] == value) { return i }
            i = i + 1
        }
        return -1
    }

    fnc main() -> Int32 {
        const nodes: List<String> = ["127.0.0.1:__P0__", "127.0.0.1:__P1__", "127.0.0.1:__P2__"]
        const ids: List<String> = ["__I0__", "__I1__", "__I2__"]
        // The first seed is dead: the topology comes from the next one.
        const cluster: RedisCluster = RedisCluster::create(["127.0.0.1:__DEAD__", nodes[0]])
        println("refresh=" + cluster.refresh().toString())

        println("crc=" + RedisCluster::crc16("123456789").toString())
        println("slot_foo=" + RedisCluster::slotFor("foo").toString())
        println("tag=" + (RedisCluster::slotFor("{user1000}.following") == RedisCluster::slotFor("{user1000}.followers")).toString())
        println("emptytag=" + (RedisCluster::slotFor("foo{}{bar}") == RedisCluster::crc16("foo{}{bar}") % 16384).toString())

        var okWrites: Int64 = 0
        var i: Int64 = 0
        while i < 300 {
            if (cluster.set("key:" + i.toString(), "v" + i.toString()).isValue()) { okWrites = okWrites + 1 }
            i = i + 1
        }
        println("writes=" + okWrites.toString())
        var okReads: Int64 = 0
        i = 0
        while i < 300 {
            const r: RedisReply = cluster.get("key:" + i.toString())
            if (r.isValue() && r.text == "v" + i.toString()) { okReads = okReads + 1 }
            i = i + 1
        }
        println("reads=" + okReads.toString())

        var spread: Int64 = 0
        var n: Int64 = 0
        while n < 3 {
            const direct: RedisClient = RedisClient::connect("127.0.0.1", RedisSentinel::address(nodes[n]).port)
            if (direct.command(["DBSIZE"]).text.toInt() > 20) { spread = spread + 1 }
            direct.close()
            n = n + 1
        }
        println("spread=" + spread.toString())

        // A wrong slot owner is corrected by MOVED, and the topology is re-read afterwards.
        const moveSlot = RedisCluster::slotFor("key:7")
        const owner = cluster.slots[moveSlot]
        cluster.slots[moveSlot] = nodes[(indexOf(nodes, owner) + 1) % 3]
        const beforeRedirects = cluster.redirects
        const moved: RedisReply = cluster.get("key:7")
        println("moved_value=" + moved.text)
        println("moved_redirects=" + (cluster.redirects - beforeRedirects).toString())
        println("moved_stale=" + cluster.stale.toString())
        const refreshesBefore = cluster.refreshes
        cluster.get("key:8")
        println("moved_refreshed=" + (cluster.refreshes == refreshesBefore + 1).toString())
        println("moved_table_fixed=" + (cluster.slots[moveSlot] == owner).toString())

        // ASK: slot of a missing key is mid-migration; the table must NOT change.
        const askSlot = RedisCluster::slotFor("askme")
        const src = indexOf(nodes, cluster.slots[askSlot])
        const dst = (src + 1) % 3
        const srcC: RedisClient = RedisClient::connect("127.0.0.1", RedisSentinel::address(nodes[src]).port)
        const dstC: RedisClient = RedisClient::connect("127.0.0.1", RedisSentinel::address(nodes[dst]).port)
        println("importing=" + dstC.command(["CLUSTER", "SETSLOT", askSlot.toString(), "IMPORTING", ids[src]]).isValue().toString())
        println("migrating=" + srcC.command(["CLUSTER", "SETSLOT", askSlot.toString(), "MIGRATING", ids[dst]]).isValue().toString())
        const askBefore = cluster.redirects
        const askGet: RedisReply = cluster.get("askme")
        println("ask_nil=" + askGet.isNil().toString())
        println("ask_redirects=" + (cluster.redirects - askBefore).toString())
        println("ask_table_unchanged=" + (cluster.slots[askSlot] == nodes[src]).toString())
        println("ask_set=" + cluster.set("askme", "x").isValue().toString())
        srcC.command(["CLUSTER", "SETSLOT", askSlot.toString(), "NODE", ids[dst]])
        dstC.command(["CLUSTER", "SETSLOT", askSlot.toString(), "NODE", ids[dst]])
        const afterMove: RedisReply = cluster.get("askme")
        println("ask_after=" + afterMove.text)
        srcC.close()
        dstC.close()

        // Failure contract: a dead node's keys fail visibly and are never retried elsewhere.
        const victimSlot = RedisCluster::slotFor("key:1")
        const victim = indexOf(nodes, cluster.slots[victimSlot])
        const killer: RedisClient = RedisClient::connect("127.0.0.1", RedisSentinel::address(nodes[victim]).port)
        killer.sendCommand(["SHUTDOWN", "NOSAVE"])
        sleep_ms(500)
        const dead: RedisReply = cluster.get("key:1")
        println("dead_failure=" + dead.isFailure().toString())
        println("dead_stale=" + cluster.stale.toString())
        cluster.close()
        return 0
    }
}
"#;

#[test]
fn cluster_routes_redirects_and_fails_visibly() {
    if Command::new("redis-server").arg("--version").stdout(Stdio::null()).status().is_err()
        || Command::new("redis-cli").arg("--version").stdout(Stdio::null()).status().is_err()
    {
        eprintln!("SKIP: redis-server/redis-cli not installed");
        return;
    }
    let dir = std::env::temp_dir().join(format!("tinox-redis-cluster-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ports = [free_cluster_port(), free_cluster_port(), free_cluster_port()];
    let mut procs = Vec::new();
    for p in ports {
        let node_dir = dir.join(format!("n{p}"));
        std::fs::create_dir_all(&node_dir).unwrap();
        procs.push(Proc(
            Command::new("redis-server")
                .args(["--port", &p.to_string(), "--bind", "127.0.0.1", "--save", "", "--appendonly", "no"])
                .args(["--cluster-enabled", "yes", "--cluster-config-file", "nodes.conf", "--cluster-node-timeout", "3000"])
                .args(["--dir", node_dir.to_str().unwrap()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawn redis-server"),
        ));
        wait_port(p);
    }
    let addrs: Vec<String> = ports.iter().map(|p| format!("127.0.0.1:{p}")).collect();
    let created = Command::new("redis-cli")
        .args(["--cluster", "create"])
        .args(&addrs)
        .args(["--cluster-replicas", "0", "--cluster-yes"])
        .output()
        .unwrap();
    assert!(created.status.success(), "{}", String::from_utf8_lossy(&created.stdout));
    for _ in 0..100 {
        if ports.iter().all(|p| cli(*p, &["cluster", "info"]).contains("cluster_state:ok")) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let ids: Vec<String> = ports.iter().map(|p| cli(*p, &["cluster", "myid"])).collect();
    let dead = loop {
        let p = free_cluster_port();
        if !ports.contains(&p) {
            break p;
        }
    };

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap();
    let version = std::fs::read_to_string(root.join("crates/tinox-core-ext/redis/tinox.toml"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("version = ").map(|v| v.trim_matches('"').to_string()))
        .unwrap();
    std::fs::write(
        dir.join("tinox.toml"),
        format!(
            "[package]\ncompiler = \"2.8.0\"\nname = \"rediscluster\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[[dependencies]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\nversion = \"{version}\"\n\n[[overrides]]\ngroup = \"tinox.core\"\nartifactId = \"redis\"\npath = \"{}\"\n",
            root.join("crates/tinox-core-ext/redis").display()
        ),
    )
    .unwrap();
    let mut main = MAIN.replace("__DEAD__", &dead.to_string());
    for i in 0..3 {
        main = main.replace(&format!("__P{i}__"), &ports[i].to_string()).replace(&format!("__I{i}__"), &ids[i]);
    }
    std::fs::write(dir.join("Main.tnx"), main).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_tinox")).args(["run", "Main.tnx"]).current_dir(&dir).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for expected in [
        "refresh=true",
        "crc=12739",
        "slot_foo=12182",
        "tag=true",
        "emptytag=true",
        "writes=300",
        "reads=300",
        "spread=3",
        "moved_value=v7",
        "moved_redirects=1",
        "moved_stale=true",
        "moved_refreshed=true",
        "moved_table_fixed=true",
        "importing=true",
        "migrating=true",
        "ask_nil=true",
        "ask_redirects=1",
        "ask_table_unchanged=true",
        "ask_set=true",
        "ask_after=x",
        "dead_failure=true",
        "dead_stale=true",
    ] {
        assert!(stdout.lines().any(|l| l == expected), "missing `{expected}` in:\n{stdout}");
    }
    drop(procs);
    let _ = std::fs::remove_dir_all(&dir);
}
