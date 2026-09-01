use anyhow::Result;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Deserialize)]
struct Memo {
    name: String,
    content: String,
    #[serde(rename = "createTime", default)]
    #[allow(dead_code)]
    create_time: String,
    #[serde(rename = "updateTime", default)]
    #[allow(dead_code)]
    update_time: String,
}

#[derive(Debug, Deserialize)]
struct ListMemosResponse {
    memos: Vec<Memo>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Serialize)]
struct MemoPatch {
    content: String,
}

struct Autotagger {
    client: Client,
    base_url: String,
    api_token: String,
    default_tag: String,
    interval: Duration,
    re_link: Regex,
    re_image: Regex,
    re_audio: Regex,
    re_video: Regex,
    re_code: Regex,
    re_task: Regex,
    re_quote: Regex,
    re_file_ext: Regex,
    known_tlds: HashSet<&'static str>,
    known_exts: HashSet<&'static str>,
}

impl Autotagger {
    fn new(base_url: String, api_token: String, default_tag: String, interval_secs: u64) -> Self {
        let re_link = Regex::new(r"https?://[^\s\)\]]+").unwrap();
        let re_image = Regex::new(r"(?i)(<img\s|!\[.*?\]\(.*?\)|\.(png|jpe?g|gif|bmp|svg|webp|tiff?|ico|heic|heif|avif)[\s\)\]\?])").unwrap();
        let re_audio = Regex::new(r"(?i)\.(mp3|wav|ogg|m4a|flac|aac|wma|opus)[\s\)\]\?]").unwrap();
        let re_video = Regex::new(r"(?i)\.(mp4|mkv|webm|avi|mov|flv|m4v|3gp|ogv)[\s\)\]\?]").unwrap();
        let re_code = Regex::new(r"```(rust|python|js|ts|go|bash|sh|sql|yaml|json|toml)").unwrap();
        let re_task = Regex::new(r"^- \[[ x]\]").unwrap();
        let re_quote = Regex::new(r"^>").unwrap();
        let re_file_ext = Regex::new(r"(?i)\.([a-z]{2,10})(?:\s|$|\)|\]|\?)").unwrap();

        let known_tlds: HashSet<&str> = [
            "com", "org", "net", "io", "dev", "co", "me", "app", "sh", "to",
            "in", "ai", "cc", "tv", "us", "uk", "de", "fr", "jp", "ru", "cn",
            "info", "biz", "xyz", "gg", "fm", "vc", "so", "is", "it", "at",
            "im", "ie", "ly", "gs", "gl", "ge", "ac", "st", "nu", "la",
        ].into_iter().collect();

        // Whitelist of file extensions worth tagging
        let known_exts: HashSet<&str> = [
            "txt", "md", "pdf", "docx", "pptx", "xlsx", "csv", "json", "yaml", "yml",
            "toml", "xml", "html", "css", "js", "ts", "jsx", "tsx", "rs", "go",
            "py", "rb", "java", "c", "cpp", "h", "sh", "bash", "zsh", "sql",
            "r", "lua", "zig", "nim", "ex", "exs", "erl", "hs", "ml", "swift",
            "kt", "scala", "cs", "fs", "vb", "php", "pl", "pm", "raku",
            "dockerfile", "makefile", "cmake", "gradle", "sbt", "cabal",
            "gitignore", "env", "lock", "log", "ini", "cfg", "conf",
            "tar", "gz", "zip", "bz2", "xz", "tgz", "7z", "rar",
            "jpg", "jpeg", "png", "gif", "bmp", "svg", "webp", "tiff", "ico", "heic", "avif",
            "mp3", "wav", "ogg", "m4a", "flac", "aac", "opus",
            "mp4", "mkv", "webm", "avi", "mov", "flv",
            "exe", "dmg", "rpm", "deb", "apk", "msi",
            "pem", "key", "crt", "cert",
            "patch", "diff",
        ].into_iter().collect();

        Self {
            client: Client::new(),
            base_url,
            api_token,
            default_tag,
            interval: Duration::from_secs(interval_secs),
            re_link,
            re_image,
            re_audio,
            re_video,
            re_code,
            re_task,
            re_quote,
            re_file_ext,
            known_tlds,
            known_exts,
        }
    }

    fn detect_tags(&self, content: &str) -> Vec<String> {
        let mut tags = Vec::new();

        // Strip ANSI escape sequences before matching
        let re_ansi = Regex::new(r"\x1b\[[0-9;]*[a-zA-Z]").unwrap();
        let clean = re_ansi.replace_all(content, "");

        if self.re_link.is_match(&clean) {
            tags.push("link".to_string());
        }
        if self.re_image.is_match(&clean) {
            tags.push("image".to_string());
        }
        if self.re_audio.is_match(&clean) {
            tags.push("audio".to_string());
        }
        if self.re_video.is_match(&clean) {
            tags.push("video".to_string());
        }
        if self.re_code.is_match(&clean) {
            tags.push("code".to_string());
        }
        if self.re_task.is_match(&clean) {
            tags.push("task".to_string());
        }
        if self.re_quote.is_match(&clean) {
            tags.push("quote".to_string());
        }

        // Extract specific file extensions as tags (whitelist only)
        for cap in self.re_file_ext.captures_iter(&clean) {
            if let Some(ext) = cap.get(1) {
                let tag = ext.as_str().to_lowercase();
                if !tags.contains(&tag)
                    && self.known_exts.contains(tag.as_str())
                {
                    tags.push(tag);
                }
            }
        }

        if tags.is_empty() {
            tags.push(self.default_tag.clone());
        }

        tags
    }

    fn existing_hashtags(content: &str) -> HashSet<String> {
        let re = Regex::new(r"(?m)#([a-zA-Z0-9_-]+)").unwrap();
        re.captures_iter(content)
            .map(|c| c[1].to_lowercase())
            .collect()
    }

    async fn run(&self) -> Result<()> {
        info!(
            "autotagger starting: default_tag={}, interval={}s",
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

        loop {
            let mut url = format!(
                "{}/api/v1/memos?pageSize=50",
                self.base_url
            );
            if let Some(token) = &page_token {
                let encoded = token.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D");
                url.push_str(&format!("&pageToken={}", encoded));
            }

            let resp = self.client
                .get(&url)
                .bearer_auth(&self.api_token)
                .send()
                .await?;
            let resp_text = resp.text().await?;

            let resp: ListMemosResponse = match serde_json::from_str(&resp_text) {
                Ok(r) => r,
                Err(e) => {
                    warn!("failed to parse response ({}): {}", e, &resp_text[..resp_text.len().min(200)]);
                    return Ok(());
                }
            };

            for memo in resp.memos {
                if memo.content.trim().is_empty() {
                    continue;
                }

                total_scanned += 1;

                let existing = Self::existing_hashtags(&memo.content);
                let detected = self.detect_tags(&memo.content);

                let new_tags: Vec<String> = detected
                    .iter()
                    .filter(|t| !existing.contains(t.as_str()))
                    .cloned()
                    .collect();

                let has_inbox_existing = existing.contains("inbox");
                let has_other_existing = existing.iter().any(|t| t != "inbox");
                let has_non_inbox_detected = detected.iter().any(|t| t != "inbox");
                let has_non_inbox_new = new_tags.iter().any(|t| t != "inbox");
                let needs_inbox_cleanup = has_inbox_existing && (has_other_existing || has_non_inbox_detected);

                // Filter out inbox if we need cleanup or have real tags
                let final_new_tags: Vec<String> = if needs_inbox_cleanup || has_non_inbox_new {
                    new_tags.into_iter().filter(|t| t != "inbox").collect()
                } else {
                    new_tags
                };

                let mut new_content = memo.content.clone();
                let mut changed = false;
                if needs_inbox_cleanup {
                    let before = new_content.clone();
                    new_content = new_content.replace(" #inbox", "").replace("#inbox ", "").replace("#inbox", "");
                    if new_content != before {
                        changed = true;
                    }
                }

                if final_new_tags.is_empty() && !changed {
                    continue;
                }

                if !final_new_tags.is_empty() {
                    let hashtag_line = final_new_tags.iter().map(|t| format!("#{}", t)).collect::<Vec<_>>().join(" ");
                    if new_content.ends_with('\n') {
                        new_content.push_str(&hashtag_line);
                    } else {
                        new_content.push('\n');
                        new_content.push_str(&hashtag_line);
                    }
                }

                // Clean up extra blank lines from inbox removal
                while new_content.contains("\n\n\n") {
                    new_content = new_content.replace("\n\n\n", "\n\n");
                }

                if self.update_content(&memo, &new_content).await? {
                    total_tagged += 1;
                    if !final_new_tags.is_empty() {
                        info!(
                            "tagged memo {} with [{}]",
                            memo.name,
                            final_new_tags.join(", ")
                        );
                    } else {
                        info!("cleaned inbox from memo {}", memo.name);
                    }
                }
            }

            page_token = resp.next_page_token.filter(|t| !t.is_empty());
            if page_token.is_none() {
                break;
            }
        }

        if total_tagged > 0 {
            info!("tagged {} memos (scanned {} recent)", total_tagged, total_scanned);
        }
        Ok(())
    }

    async fn update_content(&self, memo: &Memo, content: &str) -> Result<bool> {
        let url = format!("{}/api/v1/{}?updateMask=content", self.base_url, memo.name);

        let payload = MemoPatch {
            content: content.to_string(),
        };

        let resp = self.client
            .patch(&url)
            .bearer_auth(&self.api_token)
            .json(&payload)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!("update failed for {}: {} - {}", memo.name, status, &body[..body.len().min(200)]);
            return Ok(false);
        }
        Ok(true)
    }

    fn is_junk_hashtag(tag: &str) -> bool {
        let t = tag.to_lowercase();
        // Single letter + digit: f0, b0, f1, b1, etc.
        if t.len() == 2 && t.chars().next().unwrap().is_ascii_alphabetic() && t.chars().nth(1).unwrap().is_ascii_digit() {
            return true;
        }
        // Two letters + digit: bold, etc. — keep those, they're real words
        // Version strings: 28475v1, 03386v1, etc.
        if Regex::new(r"^\d+v\d+$").unwrap().is_match(&t) {
            return true;
        }
        // Short alphanumeric junk: 55t, 0rc1, etc. (starts with digit)
        if t.len() <= 4 && t.chars().next().unwrap().is_ascii_digit() {
            return true;
        }
        false
    }

    async fn clean_junk_hashtags(&self) -> Result<()> {
        info!("clean mode: removing junk hashtags from all memos");
        let mut page_token: Option<String> = None;
        let mut total_cleaned = 0;

        loop {
            let mut url = format!(
                "{}/api/v1/memos?pageSize=50",
                self.base_url
            );
            if let Some(token) = &page_token {
                let encoded = token.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D");
                url.push_str(&format!("&pageToken={}", encoded));
            }

            let resp = self.client
                .get(&url)
                .bearer_auth(&self.api_token)
                .send()
                .await?;
            let resp_text = resp.text().await?;

            let resp: ListMemosResponse = match serde_json::from_str(&resp_text) {
                Ok(r) => r,
                Err(e) => {
                    warn!("failed to parse response ({}): {}", e, &resp_text[..resp_text.len().min(200)]);
                    return Ok(());
                }
            };

            for memo in resp.memos {
                if memo.content.trim().is_empty() {
                    continue;
                }

                // Find all hashtags in content and remove junk ones
                let hashtag_re = Regex::new(r"(?m)(\s*)#([a-zA-Z0-9_-]+)").unwrap();
                let mut new_content = memo.content.clone();
                let mut changed = false;

                // Collect junk hashtags to remove (iterate in reverse to maintain positions)
                let mut removals: Vec<(usize, usize)> = Vec::new();
                for cap in hashtag_re.captures_iter(&memo.content) {
                    let tag = &cap[2];
                    if Self::is_junk_hashtag(tag) {
                        removals.push((cap.get(0).unwrap().start(), cap.get(0).unwrap().end()));
                    }
                }

                // Remove in reverse order to preserve positions
                for (start, end) in removals.into_iter().rev() {
                    new_content.drain(start..end);
                    changed = true;
                }

                // Clean up multiple blank lines that might result from removal
                if changed {
                    let multi_nl = Regex::new(r"\n{3,}").unwrap();
                    new_content = multi_nl.replace_all(&new_content, "\n\n").to_string();

                    if self.update_content(&memo, &new_content).await? {
                        total_cleaned += 1;
                        info!("cleaned memo {}", memo.name);
                    }
                }
            }

            page_token = resp.next_page_token.filter(|t| !t.is_empty());
            if page_token.is_none() {
                break;
            }
        }

        info!("cleaned {} memos", total_cleaned);
        Ok(())
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

    let clean_mode = std::env::args().any(|a| a == "--clean");

    let autotagger = Autotagger::new(base_url, api_token, default_tag, interval);

    if clean_mode {
        autotagger.clean_junk_hashtags().await
    } else {
        autotagger.run().await
    }
}
