//! Best-effort browser opening for `dev --open`. No browser process is started until HTTP is
//! healthy; the node keeps running even on a headless machine or if the opener fails.
use std::{net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr}, time::Duration};

fn origin(mut bind: SocketAddr) -> String {
    if bind.ip().is_unspecified() {
        bind.set_ip(if bind.is_ipv6() { IpAddr::V6(Ipv6Addr::LOCALHOST) } else { IpAddr::V4(Ipv4Addr::LOCALHOST) });
    }
    format!("http://{bind}")
}

async fn ready_url(client: &reqwest::Client, origin: &str) -> Option<String> {
    if !client.get(format!("{origin}/api/health")).send().await.ok()?.status().is_success() {
        return None;
    }
    let response = client.get(format!("{origin}/api/status")).send().await.ok()?;
    if !response.status().is_success() { return None; }
    let status: serde_json::Value = response.json().await.ok()?;
    // Only select a local, fixed path: never execute a URL supplied by an RPC response.
    match status.get("wallet")? {
        serde_json::Value::Null => Some(format!("{origin}/")),
        serde_json::Value::String(_) => Some(format!("{origin}/wallet/")),
        _ => None,
    }
}

async fn open_url(url: String) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = tokio::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = tokio::process::Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = tokio::process::Command::new("xdg-open");
    let status = tokio::time::timeout(Duration::from_secs(10), command.arg(&url)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null()).kill_on_drop(true).status()).await??;
    if status.success() { Ok(()) } else { Err(std::io::Error::other(format!("opener exited with {status}"))) }
}

async fn try_open_when_ready<F, Fut>(bind: SocketAddr, opener: F) -> Result<(), String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<()>>,
{
    let origin = origin(bind);
    // If readiness fails, offer the explorer. Once selected, preserve the exact attempted URL.
    let mut fallback = format!("{origin}/");
    let result = async {
        let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2)).build()?;
        let url = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(url) = ready_url(&client, &origin).await { return url; }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }).await?;
        // Print the chosen link even if a platform opener reports success but shows no window.
        println!("Open in your browser: {url}");
        fallback = url.clone();
        opener(url).await?;
        Ok::<_, anyhow::Error>(())
    }.await;
    result.map_err(|error| format!("Could not open a browser ({error}). Open {fallback} in your browser; the node is still running."))
}

pub async fn open_when_ready(bind: SocketAddr) {
    if let Err(message) = try_open_when_ready(bind, open_url).await {
        eprintln!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::get, Router, Json, http::StatusCode};

    #[test]
    fn wildcard_addresses_use_loopback() {
        assert_eq!(origin("0.0.0.0:7071".parse().unwrap()), "http://127.0.0.1:7071");
        assert_eq!(origin("[::]:7072".parse().unwrap()), "http://[::1]:7072");
    }

    #[tokio::test]
    async fn waits_for_health_and_selects_wallet_or_explorer() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let healthy = Arc::new(AtomicBool::new(false));
        let wallet = Arc::new(AtomicBool::new(true));
        let app = Router::new()
            .route("/api/health", get({ let healthy = healthy.clone(); move || { let healthy = healthy.clone(); async move {
                if healthy.load(Ordering::SeqCst) { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }
            }}}))
            .route("/api/status", get({ let wallet = wallet.clone(); move || { let wallet = wallet.clone(); async move {
                Json(serde_json::json!({"wallet": if wallet.load(Ordering::SeqCst) { Some("/wallet/") } else { None }}))
            }}}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = origin(listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(ready_url(&client, &origin).await, None);
        healthy.store(true, Ordering::SeqCst);
        assert_eq!(ready_url(&client, &origin).await, Some(format!("{origin}/wallet/")));
        wallet.store(false, Ordering::SeqCst);
        assert_eq!(ready_url(&client, &origin).await, Some(format!("{origin}/")));
        server.abort();
    }

    #[tokio::test]
    async fn failed_opener_reports_the_selected_wallet_or_explorer_url() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let wallet = Arc::new(AtomicBool::new(true));
        let app = Router::new()
            .route("/api/health", get(|| async { StatusCode::OK }))
            .route("/api/status", get({ let wallet = wallet.clone(); move || {
                let wallet = wallet.clone(); async move {
                    Json(serde_json::json!({"wallet": if wallet.load(Ordering::SeqCst) { Some("/wallet/") } else { None }}))
                }
            }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bind = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for (available, path) in [(true, "/wallet/"), (false, "/")] {
            wallet.store(available, Ordering::SeqCst);
            let expected = format!("{}{path}", origin(bind));
            let message = try_open_when_ready(bind, |url| async move {
                assert_eq!(url, expected);
                Err(std::io::Error::other("headless test opener"))
            }).await.unwrap_err();
            assert_eq!(message, format!("Could not open a browser (headless test opener). Open {}{path} in your browser; the node is still running.", origin(bind)));
            assert!(reqwest::Client::new().get(format!("{}/api/health", origin(bind))).send().await.unwrap().status().is_success());
        }
        server.abort();
    }

}
