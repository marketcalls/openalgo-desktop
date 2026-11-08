//! docs-35: the Profile page's Server Status shows the desktop's own parts
//! with their real state, read from Server Settings; the broker settings no
//! longer carry the web's Flask, WebSocket and ZeroMQ entries, each always
//! "Running".

use super::*;

fn part<'a>(parts: &'a [Value], name: &str) -> &'a Value {
    parts
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("no {} in {:?}", name, parts))
}

#[tokio::test]
async fn server_status_shows_each_part_with_its_real_state() {
    let h = H::new();
    h.setup();
    let (cookie, _) = h.session(true);
    *h.ctx().server_status.write() = crate::state::ServerStatus::Running {
        host: "127.0.0.1".into(),
        port: 5500,
    };
    *h.ctx().feed_status.write() = crate::state::ServerStatus::PortInUse {
        port: 8766,
        message: "Port 8766 is already used by another program.".into(),
    };
    let (s, v) = h
        .json(with_session(get("/settings/api/server"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    let parts = v["data"]["components"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let app = part(&parts, "App and API");
    assert_eq!(
        (&app["state"], &app["label"], &app["address"]),
        (&json!("ok"), &json!("Running"), &json!("127.0.0.1:5500"))
    );
    let feed = part(&parts, "Market data server");
    assert_eq!(
        (&feed["state"], &feed["label"]),
        (&json!("problem"), &json!("Port in use"))
    );
    assert_eq!(
        feed["detail"],
        "Port 8766 is already used by another program."
    );
    let broker = part(&parts, "Broker market data");
    assert_eq!(
        (&broker["state"], &broker["label"]),
        (&json!("off"), &json!("Not signed in"))
    );
    part(&parts, "Order checks");
    part(&parts, "Health checks");

    let (s, v) = h
        .json(with_session(get("/api/broker/credentials"), &cookie, None))
        .await;
    assert_eq!(s, StatusCode::OK, "{}", v);
    assert!(v["data"].get("server_status").is_none(), "{}", v);
    let text = v.to_string();
    for web_only in ["flask", "zmq", "5555"] {
        assert!(!text.contains(web_only), "{}", web_only);
    }
    h.ctx().runtime.teardown(h.ctx()).await;
}

/// A port inside a range Windows reserved (Hyper-V, WSL, Docker, the NAT
/// service) fails the bind with access denied (WSAEACCES, os error 10013),
/// not "address in use". A trader on 2026-11-06 saw only "could not open
/// port 5000" and a Zerodha login that looped back to the login page. The
/// message names the cause and the fix; a port in use keeps its own, and
/// any other failure the general one.
#[test]
fn each_bind_failure_is_told_with_its_cause_and_fix() {
    use crate::server::{bind_failure, port_in_use_message};
    use crate::state::ServerStatus;
    let reserved = std::io::Error::from_raw_os_error(10013);
    let ServerStatus::Failed { message } = bind_failure(5000, &reserved) else {
        panic!("a reserved port is not a port in use");
    };
    assert!(
        message.starts_with("OpenAlgo could not start because Windows has reserved port 5000"),
        "{message}"
    );
    assert!(
        message.contains("such as 5001, and choose Try again"),
        "{message}"
    );
    assert!(
        message.contains("http://127.0.0.1:5001/zerodha/callback"),
        "{message}"
    );
    let ServerStatus::Failed { message } = bind_failure(5001, &reserved) else {
        panic!("a reserved port is not a port in use");
    };
    assert!(message.contains("such as 5002"), "{message}");

    let in_use = std::io::Error::from(std::io::ErrorKind::AddrInUse);
    assert_eq!(
        bind_failure(5000, &in_use),
        ServerStatus::PortInUse {
            port: 5000,
            message: port_in_use_message(5000),
        }
    );
    let other = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    assert_eq!(
        bind_failure(5000, &other),
        ServerStatus::Failed {
            message: "OpenAlgo could not open port 5000. Choose a different port and try again."
                .into(),
        }
    );
}
