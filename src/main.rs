use anyhow::Result;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
struct Attachment {
    #[serde(default)]
    filename: String,
    #[serde(rename = "type", default)]
    mime_type: String,
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct Memo {
    name: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(rename = "createTime", default)]
    #[allow(dead_code)]
    create_time: String,
    #[serde(rename = "updateTime", default)]
    #[allow(dead_code)]
    update_time: String,
    #[serde(default)]
    attachments: Vec<Attachment>,
}

#[derive(Debug, Deserialize)]
struct ListMemosResponse {
    memos: Vec<Memo>,
    #[serde(rename = "nextPageToken")]
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
    default_tag: String,
    interval: Duration,
    re_image: Regex,
    re_photo: Regex,
    re_audio: Regex,
    re_video: Regex,
    re_code: Regex,
    re_task: Regex,
    re_quote: Regex,
    re_bookmark: Regex,
}

impl Autotagger {
    fn new(base_url: String, api_token: String, default_tag: String, interval_secs: u64) -> Self {
        let re_image = Regex::new(r"!\[[^\]]*\]\([^)]+\)").unwrap();
        let re_photo = Regex::new(r"(?i)photo").unwrap();
        let re_audio = Regex::new(r"(?i)\.(mp3|wav|ogg|m4a|flac|aac|wma|opus)(\s|$|\))").unwrap();
        let re_video = Regex::new(r"(?i)\.(mp4|mkv|webm|avi|mov|flv|m4v|3gp|ogv)(\s|$|\))").unwrap();
        let re_code = Regex::new(r"```(rust|python|js|ts|go|bash|sh|sql|yaml|json|toml)").unwrap();
        let re_task = Regex::new(r"^- \[[ x]\]").unwrap();
        let re_quote = Regex::new(r"^>").unwrap();
        let re_bookmark = Regex::new(r"^\[.*\]\(https?://\)").unwrap();

        Self {
            client: Client::new(),
            base_url,
            api_token,
            default_tag,
            interval: Duration::from_secs(interval_secs),
            re_image,
            re_photo,
            re_audio,
            re_video,
            re_code,
            re_task,
            re_quote,
            re_bookmark,
        }
    }

    fn has_image_attachment(&self, memo: &Memo) -> bool {
        // Check attachments already present in ListMemos (if any)
        for att in &memo.attachments {
            let mime = att.mime_type.to_lowercase();
            let fname = att.filename.to_lowercase();
            if mime.starts_with("image/") || fname.ends_with(".jpg") || fname.ends_with(".jpeg") || fname.ends_with(".png") || fname.ends_with(".gif") || fname.ends_with(".webp") || fname.ends_with(".heic") || fname.ends_with(".bmp") {
                return true;
            }
        }
        false
    }

    async fn fetch_has_image_attachment(&self, memo_name: &str) -> Result<bool> {
        let url = format!("{}/api/v1/{}/attachments", self.base_url, memo_name);
        let resp = self.client.get(&url).bearer_auth(&self.api_token).send().await?;
        if !resp.status().is_success() { return Ok(false); }
        let text = resp.text().await?.to_lowercase();
        Ok(text.contains("image/") || text.contains(".jpg") || text.contains(".jpeg") || text.contains(".png") || text.contains(".gif") || text.contains(".webp") || text.contains(".heic") || text.contains(".bmp"))
    }

    fn detect_tags(&self, content: &str) -> Vec<String> {
        let mut tags = Vec::new();

        if self.re_image.is_match(content) || self.re_photo.is_match(content) {
            tags.push("image".to_string());
        }
        if self.re_audio.is_match(content) {
            tags.push("audio".to_string());
        }
        if self.re_video.is_match(content) {
            tags.push("video".to_string());
        }
        if self.re_code.is_match(content) {
            tags.push("code".to_string());
        }
        if self.re_task.is_match(content) {
            tags.push("task".to_string());
        }
        if self.re_quote.is_match(content) {
            tags.push("quote".to_string());
        }
        if self.re_bookmark.is_match(content) {
            tags.push("bookmark".to_string());
        }

        if tags.is_empty() {
            tags.push(self.default_tag.clone());
        }

        tags
    }

    async fn run(&self) -> Result<()> {
        info!(
            "autotagger starting: default_tag='{}', interval={}s",
            self.default_tag,
            self.interval.as_secs()
        );

        loop {
            if let Err(e) = self.process_once().await {
                warn!("error in autotagger loop: {}", e);
            }
            tokio::time::sleep(self.interval).await;
        }
    }

    async fn process_once(&self) -> Result<()> {
        let mut page_token: Option<String> = None;
        let mut total_tagged = 0;
        let mut total_scanned = 0;

        // Fetch all inbox-only memos (including old) to backfill file-type tags
        loop {
            let mut url = format!(
                "{}/api/v1/memos?pageSize=50",
                self.base_url
            );
            if let Some(token) = &page_token {
                url.push_str(&format!("&page_token={}", urlencoding::encode(token)));
            }

            let resp_text = self.client
                .get(&url)
                .bearer_auth(&self.api_token)
                .send()
                .await?
                .text()
                .await?;

            let resp: ListMemosResponse = match serde_json::from_str(&resp_text) {
                Ok(r) => r,
                Err(e) => {
                    warn!("failed to parse response ({}): {}", e, &resp_text[..resp_text.len().min(200)]);
                    return Ok(());
                }
            };

            for memo in resp.memos {
                // Don't skip empty content if it has an image attachment — that should be #image
                if memo.content.trim().is_empty() && !self.has_image_attachment(&memo) {
                    // still need to check via API for attachments that ListMemos didn't include
                    if !self.fetch_has_image_attachment(&memo.name).await.unwrap_or(false) {
                        continue;
                    }
                }

                total_scanned += 1;

                // Only process memos that have inbox (whether alone or with other tags) — we want to remove inbox if they also have a file-type tag
                if !memo.tags.contains(&"inbox".to_string()) {
                    continue;
                }

                let existing: HashSet<&str> = memo.tags.iter().map(|s| s.as_str()).collect();
                // If memo already has inbox plus another tag, just clean inbox (no new detection needed)
                if memo.tags.contains(&"inbox".to_string()) && memo.tags.iter().any(|t| t != "inbox") {
                    let mut cleaned: Vec<String> = memo.tags.clone();
                    cleaned.retain(|t| t != "inbox");
                    if self.update_tags(&memo, &cleaned).await? {
                        total_tagged += 1;
                        info!("cleaned inbox from memo {} -> [{}]", memo.name, cleaned.join(", "));
                    }
                    continue;
                }

                let mut detected = self.detect_tags(&memo.content);
                // Also check for image file attached (photo uploads) — anything with image/* should be #image
                if !detected.contains(&"image".to_string()) {
                    if self.has_image_attachment(&memo) || self.fetch_has_image_attachment(&memo.name).await.unwrap_or(false) {
                        detected.push("image".to_string());
                    }
                }

                let new_tags: Vec<String> = detected
                    .iter()
                    .filter(|t| !existing.contains(t.as_str()))
                    .cloned()
                    .collect();

                if new_tags.is_empty() {
                    continue;
                }

                let mut final_tags: Vec<String> = memo.tags.clone();
                final_tags.extend(new_tags.iter().cloned());
                // Remove inbox if we now have any real autotag (image/link/code/etc)
                if final_tags.iter().any(|t| t != "inbox") {
                    final_tags.retain(|t| t != "inbox");
                }

                if self.update_tags(&memo, &final_tags).await? {
                    total_tagged += 1;
                    info!(
                        "tagged memo {} with [{}]",
                        memo.name,
                        new_tags.join(", ")
                    );
                }
            }

            page_token = resp.next_page_token;
            if page_token.is_none() {
                break;
            }
        }

        if total_tagged > 0 {
            info!("tagged {} memos (scanned {} recent)", total_tagged, total_scanned);
        }
        Ok(())
    }

    async fn update_tags(&self, memo: &Memo, tags: &[String]) -> Result<bool> {
        let url = format!("{}/api/v1/{}?update_mask=tags", self.base_url, memo.name);

        let payload = UpdateMemoRequest {
            memo: MemoPatch {
                tags: tags.to_vec(),
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

    let base_url = std::env::var("MEMOS_URL").unwrap_or_else(|_| "https://memos.junilab.xyz".to_string());
    let api_token = std::env::var("MEMOS_API_TOKEN").expect("MEMOS_API_TOKEN required");
    let default_tag = std::env::var("AUTOTAG_DEFAULT_TAG").unwrap_or_else(|_| "inbox".to_string());
    let interval = std::env::var("AUTOTAG_INTERVAL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let autotagger = Autotagger::new(base_url, api_token, default_tag, interval);
    autotagger.run().await
}
