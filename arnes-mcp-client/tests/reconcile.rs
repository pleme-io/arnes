//! A pool reconciled against a changed server set keeps every unchanged
//! server's live connection, starts the new ones and drops the removed.
//!
//! The servers are this test binary: started with `ARNES_MCP_FIXTURE=<name>`
//! it answers MCP over stdio (initialize, tools/list, one `whoami` tool that
//! reports its name and pid) instead of running the tests. A kept connection
//! is proved by the pid: the same process answers before and after.

use std::io::{BufRead, Write};
use std::time::Duration;

use arnes_mcp_client::{McpPool, McpRegistry, ServerSpec};

const FIXTURE: &str = "ARNES_MCP_FIXTURE";

fn serve(name: &str) {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { return };
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let Some(id) = msg.get("id").cloned() else { continue };
        let result = match msg["method"].as_str().unwrap_or_default() {
            "initialize" => serde_json::json!({
                "protocolVersion": msg["params"]["protocolVersion"].clone(),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": name, "version": "0"}
            }),
            "tools/list" => serde_json::json!({"tools": [{
                "name": "whoami",
                "description": "names the server and its pid",
                "inputSchema": {"type": "object"}
            }]}),
            "tools/call" => serde_json::json!({
                "content": [{"type": "text", "text": format!("{name} {}", std::process::id())}]
            }),
            _ => {
                let reply = serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no such method"}});
                let _ = writeln!(out, "{reply}");
                let _ = out.flush();
                continue;
            }
        };
        let _ = writeln!(out, "{}", serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let _ = out.flush();
    }
}

fn spec(name: &str) -> ServerSpec {
    let mut s = ServerSpec::simple(name, std::env::current_exe().unwrap().display().to_string());
    s.env.insert(FIXTURE.into(), name.into());
    s
}

fn registry(specs: Vec<ServerSpec>) -> McpRegistry {
    McpRegistry { servers: specs }
}

async fn whoami(pool: &McpPool, server: &str) -> Option<String> {
    pool.call(&format!("mcp__{server}__whoami"), &serde_json::json!({})).await.ok()
}

async fn run() {
    let t = Duration::from_secs(20);
    let before = registry(vec![spec("a"), spec("b")]);
    let pool = McpPool::connect(&before, t).await.unwrap();
    assert!(pool.failures().is_empty(), "{:?}", pool.failures());
    let a_pid = whoami(&pool, "a").await.expect("a serves");
    assert!(whoami(&pool, "b").await.is_some());

    // b removed, c added, a untouched.
    let after = registry(vec![spec("a"), spec("c")]);
    let (next, report) = pool.reconcile(&before, &after, t, None).await;
    assert_eq!(report.kept, ["a"]);
    assert_eq!(report.started, ["c"]);
    assert_eq!(report.stopped, ["b"]);
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(whoami(&next, "a").await.as_deref(), Some(a_pid.as_str()), "a kept its connection (same process)");
    assert!(whoami(&next, "c").await.is_some_and(|s| s.starts_with("c ")), "c started");
    assert!(whoami(&next, "b").await.is_none(), "b is gone");
    let names: Vec<&str> = next.tools().iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["mcp__a__whoami", "mcp__c__whoami"], "the tool list stays sorted and whole");
    // The old pool, still held by a turn in flight, still answers.
    assert!(whoami(&pool, "b").await.is_some(), "a running turn keeps the pool it started with");

    // A changed spec restarts that server alone.
    let mut changed = spec("a");
    changed.args.push("--changed".into());
    let again = registry(vec![changed, spec("c")]);
    let c_pid = whoami(&next, "c").await.unwrap();
    let (third, report) = next.reconcile(&after, &again, t, None).await;
    assert_eq!((report.kept.as_slice(), report.started.as_slice()), (["c".to_owned()].as_slice(), ["a".to_owned()].as_slice()));
    assert_ne!(whoami(&third, "a").await.unwrap(), a_pid, "a restarted");
    assert_eq!(whoami(&third, "c").await.unwrap(), c_pid, "c untouched");

    // A new server that cannot start is reported, and the rest serve.
    let broken = registry(vec![spec("c"), ServerSpec::simple("ghost", "/nonexistent/arnes-mcp")]);
    let (fourth, report) = third.reconcile(&again, &broken, t, None).await;
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.stopped, ["a"]);
    assert_eq!(whoami(&fourth, "c").await.unwrap(), c_pid);
    let (_, noop) = fourth.reconcile(&broken, &broken, t, None).await;
    assert!(noop.is_noop(), "an unchanged set touches nothing, a failed server is not retried: {noop:?}");

    arrivals_fill_a_pool_one_at_a_time(t).await;
    a_lazy_server_starts_on_its_first_call_once(t).await;
}

async fn arrivals_fill_a_pool_one_at_a_time(t: Duration) {
    let empty = McpPool::empty();
    let a = McpPool::connect_server(spec("a"), t, None).await;
    assert!(a.error().is_none());
    let one = empty.with_arrival(a);
    assert_eq!(one.servers(), ["a"]);
    assert!(empty.servers().is_empty(), "the pool it came from is untouched");
    let ghost = McpPool::connect_server(ServerSpec::simple("ghost", "/nonexistent/arnes-mcp"), t, None).await;
    assert!(ghost.error().is_some());
    let two = one.with_arrival(ghost).with_arrival(McpPool::connect_server(spec("b"), t, None).await);
    assert_eq!(two.servers(), ["a", "b"]);
    assert_eq!(two.failures().len(), 1);
    let names: Vec<&str> = two.tools().iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["mcp__a__whoami", "mcp__b__whoami"]);
    let p = two.probe_one("a", Duration::from_secs(5)).await;
    assert!(p.up && p.tools == 1, "{p:?}");
    let a_pid = whoami(&two, "a").await.unwrap();
    let again = two.with_arrival(McpPool::connect_server(spec("a"), t, None).await);
    assert_ne!(whoami(&again, "a").await.unwrap(), a_pid, "a re-arrival replaces the old connection");
    assert_eq!(again.tools().len(), 2, "and does not duplicate its tools");
    assert!(!again.without("a").servers().contains(&"a"));
}

async fn a_lazy_server_starts_on_its_first_call_once(t: Duration) {
    let mut lazy = spec("z");
    lazy.lazy = true;
    let reg = registry(vec![spec("a"), lazy]);
    let pool = McpPool::connect(&reg, t).await.unwrap();
    assert_eq!(pool.servers(), ["a"], "z is not started with the pool");
    assert_eq!(pool.dormant(), ["z"]);
    let (first, second) = tokio::join!(whoami(&pool, "z"), whoami(&pool, "z"));
    assert_eq!(first, second, "concurrent first calls share one start");
    assert!(first.is_some_and(|s| s.starts_with("z ")));
    assert_eq!(pool.servers(), ["a", "z"]);
    let unknown = pool.call("mcp__z__nope", &serde_json::json!({})).await.unwrap_err().to_string();
    assert!(unknown.contains("mcp__z__whoami"), "an unknown tool on a woken server lists its tools: {unknown}");
    let z_pid = whoami(&pool, "z").await.unwrap();
    let (kept, report) = pool.reconcile(&reg, &reg, t, None).await;
    assert!(report.is_noop(), "{report:?}");
    assert_eq!(whoami(&kept, "z").await.unwrap(), z_pid, "a woken lazy server keeps its connection across a reconcile");
}

fn main() {
    if let Ok(name) = std::env::var(FIXTURE) {
        serve(&name);
        return;
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(run());
    println!("reconcile: ok");
}
