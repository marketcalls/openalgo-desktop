//! whatsapp-rust's HTTP needs (app version lookup, media CDN) over the
//! app's shared `reqwest::Client`, instead of the crate's ureq client. The
//! URLs come from the crate and WhatsApp's servers, never from a user.

use whatsapp_rust::http::{HttpClient, HttpRequest, HttpResponse};

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub struct ReqwestHttp {
    http: reqwest::Client,
    /// Tests: refuse every request so nothing reaches WhatsApp.
    blocked: bool,
}

impl ReqwestHttp {
    pub fn new(http: reqwest::Client, blocked: bool) -> Self {
        Self { http, blocked }
    }
}

#[async_trait::async_trait]
impl HttpClient for ReqwestHttp {
    async fn execute(&self, req: HttpRequest) -> anyhow::Result<HttpResponse> {
        if self.blocked {
            anyhow::bail!("outbound WhatsApp HTTP is disabled");
        }
        let method = reqwest::Method::from_bytes(req.method.as_bytes())?;
        let mut b = self.http.request(method, &req.url).timeout(TIMEOUT);
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        if let Some(body) = req.body {
            b = b.body(body.to_vec());
        }
        let resp = b
            .send()
            .await
            .map_err(|e| anyhow::anyhow!(e.without_url()))?;
        let status_code = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!(e.without_url()))?
            .to_vec();
        Ok(HttpResponse { status_code, body })
    }
}
