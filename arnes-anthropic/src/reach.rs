use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::catalog::Auth;

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub name: String,
    pub url: String,
    pub key: Option<Arc<cofre_secret::Secret>>,
    pub auth: Auth,
    pub timeout: Duration,
    pub max_bytes: usize,
}

pub type Call = Arc<dyn Fn(&str, &str) -> Result<String, String> + Send + Sync>;

#[must_use]
pub fn reach(endpoints: Vec<Endpoint>, handle: tokio::runtime::Handle) -> Call {
    let client = reqwest::Client::new();
    Arc::new(move |name: &str, body: &str| {
        let Some(e) = endpoints.iter().find(|e| e.name == name) else {
            let declared: Vec<&str> = endpoints.iter().map(|e| e.name.as_str()).collect();
            return Err(format!("no endpoint named {name:?}; declared: {declared:?}"));
        };
        let started = Instant::now();
        let sent = handle.block_on(async {
            let mut req = client.post(&e.url).timeout(e.timeout).header("content-type", "application/json").body(body.to_owned());
            if let Some(key) = &e.key {
                req = match e.auth {
                    Auth::XApiKey => req.header("x-api-key", key.expose()),
                    Auth::Bearer => req.bearer_auth(key.expose()),
                };
            }
            let resp = req.send().await?;
            let status = resp.status().as_u16();
            let bytes = resp.bytes().await?;
            Ok::<_, reqwest::Error>((status, bytes))
        });
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let answer = match sent {
            Ok((status, bytes)) if bytes.len() > e.max_bytes => serde_json::json!({"status": status, "verdict": "too-large", "ms": ms, "body": ""}),
            Ok((status, bytes)) => serde_json::json!({
                "status": status,
                "verdict": handan::judge(status, None).verdict().to_string(),
                "ms": ms,
                "body": String::from_utf8_lossy(&bytes),
            }),
            Err(err) => serde_json::json!({"status": 0, "verdict": if err.is_timeout() { "timed-out" } else { "unreachable" }, "ms": ms, "body": ""}),
        };
        Ok(answer.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::one_shot_server;

    fn endpoint(name: &str, url: String, key: Option<&str>, max_bytes: usize) -> Endpoint {
        Endpoint {
            name: name.into(),
            url,
            key: key.map(|k| Arc::new(cofre_secret::Secret::new(k).unwrap())),
            auth: Auth::Bearer,
            timeout: Duration::from_secs(2),
            max_bytes,
        }
    }

    fn call(endpoints: Vec<Endpoint>, name: &str, body: &str) -> Result<serde_json::Value, String> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let reach = reach(endpoints, rt.handle().clone());
        let name = name.to_owned();
        let body = body.to_owned();
        std::thread::spawn(move || reach(&name, &body)).join().unwrap().map(|t| serde_json::from_str(&t).unwrap())
    }

    #[test]
    fn a_declared_endpoint_answers_with_status_verdict_and_body_and_the_key_stays_out() {
        let (url, served) = one_shot_server("200 OK", r#"{"answers":{}}"#);
        let v = call(vec![endpoint("judge", url, Some("sk-ts"), 1024)], "judge", "{\"q\":1}").unwrap();
        assert_eq!((v["status"].as_u64(), v["verdict"].as_str(), v["body"].as_str()), (Some(200), Some("success"), Some(r#"{"answers":{}}"#)));
        let request = served.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("authorization: bearer sk-ts") && request.contains("{\"q\":1}"), "{request}");
        assert!(!v.to_string().contains("sk-ts"), "the key never comes back to blue");
    }

    #[test]
    fn an_undeclared_endpoint_names_the_declared_ones() {
        let err = call(vec![endpoint("judge", "http://127.0.0.1:9".into(), None, 1024)], "other", "{}").unwrap_err();
        assert!(err.contains("\"other\"") && err.contains("judge"), "{err}");
    }

    #[test]
    fn an_oversized_reply_is_withheld_and_a_dead_endpoint_is_unreachable() {
        let (url, served) = one_shot_server("200 OK", "0123456789");
        let v = call(vec![endpoint("j", url, None, 4)], "j", "{}").unwrap();
        assert_eq!((v["verdict"].as_str(), v["body"].as_str()), (Some("too-large"), Some("")));
        served.join().unwrap();
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let v = call(vec![endpoint("j", format!("http://{dead}"), None, 4)], "j", "{}").unwrap();
        assert_eq!((v["status"].as_u64(), v["verdict"].as_str()), (Some(0), Some("unreachable")));
    }
}
