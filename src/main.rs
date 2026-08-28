use anyhow::Result;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{info, warn, error};

#[derive(Debug, Deserialize)]
struct Memo {
    name: String,
    content: String,
    tags: Vec<String>,
    create_time: String,
    update_time: String,
}

#[derive(Debug, Deserialize)]
struct ListMemosResponse {
    memos: Vec<Memo>,
    next_page_token: Option<String>,
}

#[derive(Debug, Serialize)]
struct UpdateMemoRequest {
    memo: MemoPatch,
    update_mask: String,
}

#[derive(Debug, Serialize)]
struct MemoPatch {
    tags: Vec<String>,
}

struct Autotagger {
    client: Client,
    base_url: String,
    api_token: String,
    tag: String,
    interval: Duration,
}

impl Autotagger {
    fn new(base_url: String, api_token: String, tag: String, interval_secs: u64) -> Self {
        Self {
            client: Client::new(),
            base_url,
            api_token,
            tag,
            interval: Duration::from_secs(interval_secs),
        }
    }

    async fn run(&self) -> Result<()> {
        info!("Starting autotagger with tag '{}', interval {}s", self.tag, self.interval.as_secs());
        
        loop {
            if let Err(e) = self.process_once().await {
                error!("Error in autotagger loop: {}", e);
            }
            tokio::time::sleep(self.interval).await;
        }
    }

    async fn process_once(&self) -> Result<()> {
        let mut page_token = None;
        let mut total_tagged = 0;

        loop {
            let mut url = format!("{}/api/v1/memos?page_size=100", self.base_url);
            if let Some(token) = &page_token {
                url.push_str(&format!("&page_token={}", token));
            }

            let resp: ListMemosResponse = self.client
                .get(&url)
                .bearer_auth(&self.api_token)
                .send()
                .await?
                .json()
                .await?;

            for memo in resp.memos {
                if memo.tags.is_empty() && !memo.content.trim().is_empty() {
                    if self.tag_memo(&memo).await? {
                        total_tagged += 1;
                        info!("Tagged memo {} with #{}", memo.name, self.tag);
                    }
                }
            }

            page_token = resp.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        if total_tagged > 0 {
            info!("Tagged {} memos in this run", total_tagged);
        }
        Ok(())
    }

    async fn tag_memo(&self, memo: &Memo) -> Result<bool> {
        let url = format!("{}/api/v1/{}?update_mask=tags", self.base_url, memo.name);
        
        let payload = UpdateMemoRequest {
            memo: MemoPatch {
                tags: vec![self.tag.clone()],
            },
            update_mask: "tags".to_string(),
        };

        let resp = self.client
            .patch(&url)
            .bearer_auth(&self.api_token)
            .json(&payload)
            .send()
            .await?;

        Ok(resp.status().is_success())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let base_url = std::env::var("MEMOS_URL").expect("MEMOS_URL required");
    let api_token = std::env::var("MEMOS_API_TOKEN").expect("MEMOS_API_TOKEN required");
    let tag = std::env::var("AUTOTAG_TAG").unwrap_or_else(|_| "inbox".to_string());
    let interval = std::env::var("AUTOTAG_INTERVAL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let autotagger = Autotagger::new(base_url, api_token, tag, interval);
    autotagger.run().await
}