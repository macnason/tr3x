//! Where a thread opens. The panes run on the Mac that runs T3, but a click on a link is
//! opened by the device it's made on, which can be a laptop or phone looking at the same Rex
//! session. So a thread's link is T3's web app at the address T3 serves on the tailnet, which
//! every device on it can open, else T3 Code's own `t3code://` link, which opens only where
//! T3 Code is installed. `T3REX_WEB_URL` sets the web address, and `none` keeps the app link.

use serde_json::Value;

/// The web app's address: `T3REX_WEB_URL`, else the tailnet endpoint T3 advertises in its
/// server config, HTTPS first.
fn web_origin(config: Option<&Value>, set: Option<&str>) -> Option<String> {
    if let Some(set) = set.map(str::trim).filter(|s| !s.is_empty()) {
        return (set != "none").then(|| set.trim_end_matches('/').to_string());
    }
    let endpoints = config?["directEndpoints"].as_array()?;
    let tailnet: Vec<&str> = endpoints
        .iter()
        .filter(|e| e["kind"] == "tailnet")
        .filter_map(|e| e["httpBaseUrl"].as_str())
        .collect();
    tailnet
        .iter()
        .find(|u| u.starts_with("https://"))
        .or(tailnet.first())
        .map(|u| u.trim_end_matches('/').to_string())
}

/// The thread's link, as T3's web app routes it (`/$environmentId/$threadId`) or as T3 Code's
/// `t3code://threads/…` deep link, which the app turns into the same route.
pub fn thread(config: Option<&Value>, environment: &str, thread: &str) -> String {
    let set = std::env::var("T3REX_WEB_URL").ok();
    match web_origin(config, set.as_deref()) {
        Some(origin) => format!("{origin}/{environment}/{thread}"),
        None => format!("t3code://threads/{environment}/{thread}"),
    }
}

/// `text` as an OSC 8 link to `url`.
pub fn osc8(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// Whether every Rex app attached to the server is on this Mac, so whoever typed a command
/// is here too. A laptop or phone attached over the tailnet makes that unknown.
pub async fn only_here() -> bool {
    let rex = crate::rex::Rex::find();
    let Ok(out) = tokio::process::Command::new(&rex.bin)
        .args(["api", "call", "client.list", "{}"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    else {
        return false;
    };
    let Ok(list) = serde_json::from_slice::<Value>(&out.stdout) else {
        return false;
    };
    all_local(&list)
}

fn all_local(list: &Value) -> bool {
    list["clients"].as_array().is_some_and(|clients| {
        clients
            .iter()
            .filter(|c| c["info"]["kind"] == "app")
            .all(|c| c["principal"]["transport"] == "unix")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_web_app_is_on_the_tailnet_https_first() {
        let config = json!({"directEndpoints": [
            {"kind": "lan", "httpBaseUrl": "http://192.168.1.2:3773/"},
            {"kind": "tailnet", "httpBaseUrl": "http://100.1.2.3:3773/"},
            {"kind": "tailnet", "httpBaseUrl": "https://mini.example.ts.net/"},
        ]});
        assert_eq!(
            web_origin(Some(&config), None).as_deref(),
            Some("https://mini.example.ts.net")
        );
        assert_eq!(
            web_origin(Some(&config), Some("https://t3.example/")).as_deref(),
            Some("https://t3.example")
        );
        assert_eq!(web_origin(Some(&config), Some("none")), None);
        // No tailnet: only T3 Code's own link can open it.
        let lan = json!({"directEndpoints": [{"kind": "lan", "httpBaseUrl": "http://192.168.1.2:3773/"}]});
        assert_eq!(web_origin(Some(&lan), None), None);
        assert_eq!(web_origin(None, None), None);
    }

    #[test]
    fn remote_apps_are_not_here() {
        let mini = json!({"info": {"kind": "app"}, "principal": {"transport": "unix"}});
        let cli = json!({"info": {"kind": "cli"}, "principal": {"transport": "tailscale"}});
        let air = json!({"info": {"kind": "app"}, "principal": {"transport": "tailscale"}});
        assert!(all_local(&json!({"clients": [mini, cli]})));
        assert!(!all_local(&json!({"clients": [mini, air]})));
    }
}
