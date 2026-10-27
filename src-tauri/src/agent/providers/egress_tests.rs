//! The Agent's HTTP client never reaches the link-local blocks, however a
//! name resolves or a provider redirects; a server on this computer or the
//! local network stays reachable. No real name lookup: a test lookup maps
//! names to addresses.

use super::egress::*;
use crate::agent::run::client::http_client_with;
use crate::agent::run::errors::{provider_message, read_genai_error};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Names mapped to addresses, as a lookup.
struct Table(Arc<HashMap<String, Vec<SocketAddr>>>);

impl Lookup for Table {
    fn lookup(&self, host: String) -> Looking {
        let found = self.0.get(&host).cloned();
        Box::pin(async move {
            found.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such name"))
        })
    }
}

fn addr(s: &str) -> SocketAddr {
    format!("{}:0", s)
        .parse()
        .or_else(|_| format!("[{}]:0", s).parse())
        .unwrap()
}

/// A local server answering every request with `answer`.
async fn serve(answer: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(answer.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (port, task)
}

#[test]
fn the_link_local_blocks_are_refused_however_written() {
    for refused in [
        "169.254.169.254",
        "169.254.0.1",
        "169.254.255.255",
        "fe80::1",
        "febf::ffff",
        "fd00:ec2::254",
        "fd00:ec2:1::1",
        "::ffff:169.254.169.254",
        "::169.254.169.254",
        "64:ff9b::169.254.169.254",
        "64:ff9b::a9fe:1",
    ] {
        assert!(is_refused(addr(refused).ip()), "{}", refused);
    }
    for allowed in [
        "127.0.0.1",
        "10.0.0.5",
        "192.168.1.2",
        "172.16.0.1",
        "169.255.0.1",
        "::1",
        "::",
        "fec0::1",
        "fd00:ec3::1",
        "::ffff:127.0.0.1",
        "64:ff9b::7f00:1",
        "2001:db8::1",
    ] {
        assert!(!is_refused(addr(allowed).ip()), "{}", allowed);
    }
    let mixed = vec![addr("169.254.169.254"), addr("127.0.0.1")];
    assert_eq!(keep_allowed(mixed).unwrap(), vec![addr("127.0.0.1")]);
    assert_eq!(
        keep_allowed(vec![addr("::ffff:169.254.1.1"), addr("fe80::2")]),
        Err(EndpointRefused)
    );
}

/// The resolver refuses a name that resolves only into the blocks before
/// anything connects, and the run says it in the guard's words; a name on
/// this computer is reached.
#[tokio::test]
async fn a_name_resolving_to_the_metadata_service_is_refused_at_connect() {
    let (port, task) =
        serve("HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok").await;
    let names: HashMap<String, Vec<SocketAddr>> = [
        ("meta.test".to_string(), vec![addr("169.254.169.254")]),
        (
            "mapped.test".to_string(),
            vec![addr("::ffff:169.254.169.254")],
        ),
        ("local.test".to_string(), vec![addr("127.0.0.1")]),
    ]
    .into_iter()
    .collect();
    let names = Arc::new(names);
    let client = http_client_with(GuardedResolver(Table(names.clone()))).unwrap();
    let ok = client
        .get(format!("http://local.test:{}/", port))
        .send()
        .await
        .unwrap();
    assert_eq!(
        ok.status().as_u16(),
        200,
        "a local server is reachable by name"
    );
    for name in ["meta.test", "mapped.test"] {
        let err = client
            .get(format!("http://{}:{}/", name, port))
            .send()
            .await
            .unwrap_err();
        let genai_err = genai::Error::WebModelCall {
            model_iden: genai::ModelIden::new(genai::adapter::AdapterKind::OpenAI, "x"),
            webc_error: genai::webc::Error::Reqwest(err),
        };
        assert!(refused(&genai_err), "{}", name);
        assert_eq!(
            provider_message(&read_genai_error(&genai_err), "openai_compatible"),
            super::base_url::METADATA
        );
    }
    task.abort();
}

/// Through genai, as a run sends it: the stream never opens.
#[tokio::test]
async fn a_model_request_to_a_refused_name_answers_with_the_guards_sentence() {
    use genai::chat::ChatRequest;
    use genai::resolver::{AuthData, Endpoint};
    let names: HashMap<String, Vec<SocketAddr>> =
        [("provider.test".to_string(), vec![addr("169.254.10.10")])]
            .into_iter()
            .collect();
    let client = genai::Client::builder()
        .with_reqwest(http_client_with(GuardedResolver(Table(Arc::new(names)))).unwrap())
        .build()
        .unwrap();
    let target = genai::ServiceTarget {
        endpoint: Endpoint::from_owned("http://provider.test:9/v1/".to_string()),
        auth: AuthData::from_single("k"),
        model: genai::ModelIden::new(genai::adapter::AdapterKind::OpenAI, "m"),
    };
    let outcome = client
        .exec_chat_stream(target, ChatRequest::from_user("ping"), None)
        .await;
    let err = match outcome {
        Err(e) => e,
        Ok(r) => {
            use futures_util::StreamExt;
            let mut s = r.stream;
            let mut last = None;
            while let Some(ev) = s.next().await {
                if let Err(e) = ev {
                    last = Some(e);
                    break;
                }
            }
            last.expect("the stream fails")
        }
    };
    assert!(refused(&err), "{:?}", err);
}

/// A provider that redirects is not followed: the redirect is the answer.
#[tokio::test]
async fn a_redirect_is_not_followed() {
    let (port, task) = serve(
        "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .await;
    let client = crate::agent::run::client::http_client().unwrap();
    let r = client
        .get(format!("http://127.0.0.1:{}/v1/chat/completions", port))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 302);
    task.abort();
}
