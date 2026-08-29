# memos-autotagger

Rust daemon that auto-tags your [Memos](https://github.com/usememos/memos) notes by content. Runs on an interval, scans `content` + existing `tags`, and patches missing tags via the Memos API.

## Tag rules (regex)

- `image` — `![](...)` markdown images
- `audio` — `.mp3/.wav/.ogg/.m4a/.flac/...`
- `video` — `.mp4/.mkv/.webm/.mov/...`
- `code` — fenced ` ```rust/python/js/...` blocks
- `task` — `- [ ]` / `- [x]` task lists
- `quote` — `> blockquote`
- `bookmark` — `[title](https://...)` link-only notes
- fallback `default_tag` if no other match (configurable)

## Quick start

```bash
cp .env.example .env  # if present, or set vars directly
# required:
export MEMOS_URL="https://memos.example.com"
export MEMOS_TOKEN="memos_xxx"
export DEFAULT_TAG="inbox"  # optional, default: inbox
export INTERVAL_SECS=300    # optional, default: 60

cargo run --release
# or
docker build -t memos-autotagger . && docker run --env-file .env memos-autotagger
```

## How it works

1. Lists memos via `GET /api/v1/memos` with pagination (`nextPageToken`)
2. For each memo, computes `HashSet<tag>` from content regexes
3. If new tags found, `PATCH /api/v1/memos/{name}` with `updateMask=tags`

Minimal deps: `reqwest`, `regex`, `tokio`, `serde`.

## License

MIT
