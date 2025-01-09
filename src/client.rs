use std::{
    cmp::min,
    fmt::Display,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Result};
use bytes::Bytes;
use reqwest::{
    header::{HeaderValue, CONTENT_TYPE},
    IntoUrl,
};
use tokio::sync::Semaphore;

use crate::plato::notify;

pub struct Client {
    client: Arc<reqwest::Client>,
    semaphore: Arc<Semaphore>,
    sigterm: Arc<AtomicBool>,
}

pub struct Response {
    pub content_type: Option<HeaderValue>,
    pub body: Bytes,
}

impl Client {
    pub fn new(user_agent: String, concurrent_requests: usize) -> Result<Client> {
        let semaphore = Semaphore::new(min(concurrent_requests, Semaphore::MAX_PERMITS));
        let sigterm = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&sigterm))?;
        Ok(Client {
            client: Arc::new(reqwest::Client::builder().user_agent(user_agent).build()?),
            semaphore: Arc::new(semaphore),
            sigterm,
        })
    }

    pub async fn get<D: Display, U: IntoUrl>(&self, resource: D, url: U) -> Result<Response> {
        let permit = self.semaphore.acquire().await?;
        if self.sigterm.load(Ordering::Relaxed) {
            return Err(anyhow!("SIGTERM"));
        }

        notify(&format!("loading {resource}"));
        let res = self.client.get(url).send().await?;
        if self.sigterm.load(Ordering::Relaxed) {
            return Err(anyhow!("SIGTERM"));
        }

        let content_type = res.headers().get(CONTENT_TYPE).cloned();
        let body = res.bytes().await?;
        if self.sigterm.load(Ordering::Relaxed) {
            return Err(anyhow!("SIGTERM"));
        }

        drop(permit);
        Ok(Response { content_type, body })
    }
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
            semaphore: Arc::clone(&self.semaphore),
            sigterm: Arc::clone(&self.sigterm),
        }
    }
}
