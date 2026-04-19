# AnyCLI

Turn any website into structured CLI output. Declarative YAML adapters for web data extraction.

```bash
$ anycli run hackernews top --format table limit=5
by            | descendants | score | title                                          | url
--------------+-------------+-------+------------------------------------------------+--------------------------------------------
crescit_eundo | 55          | 116   | The Bromine Chokepoint                         | https://warontherocks.com/...
colesantiago  | 265         | 409   | Vercel April 2026 security incident            | https://www.bleepingcomputer.com/...

$ anycli run bilibili hot --format table limit=3
title                          | bvid         | author    | view    | danmaku
-------------------------------+--------------+-----------+---------+--------
...                            | BV1wYdWB5EVF | ...       | 3955373 | 37626
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

### Run an adapter

```bash
anycli run <adapter> <command> [--format json|table|csv|markdown] [key=value ...]
```

Examples:

```bash
# Hacker News top stories
anycli run hackernews top limit=10

# GitHub trending repos (Rust, weekly)
anycli run github-trending repos language=rust since=weekly --format table

# Wikipedia article summary
anycli run wikipedia summary title=Rust_programming_language

# Search arXiv papers
anycli run arxiv search query="large language model" limit=5

# Bilibili hot videos
anycli run bilibili hot --format markdown limit=10

# Bilibili ranking by category
anycli run bilibili ranking rid=36
```

### List available adapters

```bash
anycli list
```

### Show adapter details

```bash
anycli info hackernews
```

### Community hub

```bash
# Search for adapters
anycli search zhihu

# Install from hub
anycli install zhihu

# Update all installed adapters
anycli update
```

## Built-in Adapters

| Adapter | Commands | Source |
|---------|----------|--------|
| hackernews | top, new, item | JSON API |
| github-trending | repos | HTML scraping |
| arxiv | search, recent | XML API |
| wikipedia | summary, search | JSON API |
| bilibili | hot, search, ranking | JSON API |

## Custom Adapters

Create YAML files in `~/.anycli/adapters/`:

```yaml
name: mysite
description: "My custom adapter"
base_url: "https://api.example.com"

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
    params:
      limit:
        type: integer
        default: 10
        description: "Number of items"
```

### Adapter Schema

**Source formats:** `html`, `json`, `xml`

**Field extraction:**
- `json_path` — dot-separated path for JSON (e.g., `data.title`)
- `pattern` — regex with capture group for HTML/XML
- `default` — fallback value
- `transform` — post-processing: `strip_html`, `trim`, `decode_entities`, `to_number`

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

## Library Usage

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

## Output Formats

- **json** (default) — JSON array
- **table** — aligned columns with headers
- **csv** — comma-separated values
- **markdown** — GitHub-flavored markdown table

## License

MIT
