# AnyCLI

Turn any website into structured CLI output. Declarative YAML adapters for web data extraction.

```bash
$ anycli hackernews top --format table limit=5
┌────────────────┬─────────────┬───────┬──────────────────────────────┬──────────────────────────┐
│ by             │ descendants │ score │ title                        │ url                      │
├────────────────┼─────────────┼───────┼──────────────────────────────┼──────────────────────────┤
│ crescit_eundo  │ 55          │ 116   │ The Bromine Chokepoint       │ https://warontherocks... │
└────────────────┴─────────────┴───────┴──────────────────────────────┴──────────────────────────┘
```

## Install

```bash
cargo install anycli
```

Or build from source:

```bash
git clone https://github.com/oopos/anycli.git
cd anycli
cargo build --release
```

## Usage

`anycli <adapter> <command>` is equivalent to `anycli run <adapter> <command>`.

```bash
anycli <adapter> <command> [--format json|table|csv|markdown|yaml|plain] [--fields col,col] [key=value ...]
```

Examples:

```bash
# Hacker News top stories (limit defaults to 10)
anycli hackernews top

# GitHub trending repos (Rust, weekly)
anycli github-trending repos language=rust since=weekly --format table

# Wikipedia article summary (positional arg for the required param)
anycli wikipedia summary Rust_programming_language

# Search arXiv papers
anycli arxiv search query="large language model" limit=5

# Bilibili hot videos as markdown
anycli bilibili hot --format markdown limit=10

# Hugging Face alias
anycli hf top limit=5 --fields id,likes
```

### List / inspect adapters

```bash
anycli list
anycli list --format json
anycli info hackernews
anycli hackernews --help
anycli hackernews top --help
```

### Community hub

```bash
anycli search zhihu
anycli install zhihu
anycli update
anycli uninstall zhihu
```

### Validate a custom adapter

```bash
anycli validate ~/.anycli/adapters/mysite.yaml
```

### Shell completions

```bash
anycli completions bash > /etc/bash_completion.d/anycli
anycli completions zsh
anycli completions fish
```

## Built-in adapters

100+ adapters covering news, video, academic search, shopping, finance, and desktop apps. Run `anycli list` for the current set.

Public JSON APIs (no browser): `hackernews`, `github`, `arxiv`, `wikipedia`, `bilibili`, `reddit`, `devto`, `lobsters`, …

HTML / browser adapters: `github-trending`, `xiaohongshu`, `youtube`, `douyin`, …

Desktop CDP adapters: `cursor`, `chatgpt-app`, `discord-app`, `notion`, `doubao-app`, …

## Custom adapters

Create YAML files in `~/.anycli/adapters/`:

```yaml
name: mysite
description: "My custom adapter"
base_url: "https://api.example.com"
aliases: ["ms"]
tags: ["example"]

commands:
  hot:
    description: "Hot posts"
    url: "/api/hot?limit={limit}"
    format: json
    selector: "data.items"
    fields:
      title:
        json_path: "title"
      score:
        json_path: "score"
      url:
        json_path: "link"
        alt_paths: ["url"]
      permalink:
        template: "https://example.com/p/{id}"
    params:
      limit:
        type: integer
        default: 10
        description: "Number of items"
```

POST / GraphQL adapters can set `method: POST` and a `body` mapping. `{param}` placeholders in the body are replaced; other braces (GraphQL selections) are left intact.

Browser/desktop `evaluate` scripts can use `${{param}}` (or `{param}`) for CLI values.

### Adapter schema

**Source formats:** `html`, `json`, `xml`, `browser`, `browser_api`, `desktop`, `intercept`

**HTTP:** `method`, `headers`, `body`, `content_type`, `timeout`

**Field extraction:**
- `json_path` — dot-separated path (`data.title`, `[].eid`, `@index`)
- `alt_paths` — fallback paths
- `template` — build a value from `{field}`, JSON keys, or params
- `pattern` — regex with a capture group for HTML/XML
- `default` — fallback value
- `transform` — `strip_html`, `trim`, `decode_entities`, `to_number`, `add_one`

**Advanced: fetch_each**

For APIs that return ID lists (like Hacker News), use `fetch_each` to fetch each item individually:

```yaml
commands:
  top:
    url: "/topstories.json"
    format: json
    fields: {}
    fetch_each:
      url: "/item/{id}.json"
      format: json
      fields:
        title:
          json_path: "title"
        score:
          json_path: "score"
```

## Library usage

```rust
use anycli::{Registry, Pipeline, OutputFormat};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let registry = Registry::load()?;
    let adapter = registry.find("hackernews")?;
    let result = Pipeline::execute(&adapter, "top", &[("limit", "10")]).await?;
    println!("{}", result.format(OutputFormat::Json)?);
    Ok(())
}
```

## Output formats

- **table** (default) — Unicode box-drawing table, CJK-aware; respects `NO_COLOR`
- **json** — pretty-printed JSON array
- **csv** — comma-separated values
- **markdown** / **md** — GitHub-flavored markdown table
- **yaml** / **yml** — YAML array
- **plain** / **tsv** — tab-separated values, no headers (for piping)

## License

MIT
