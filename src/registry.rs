//! Adapter registry — discover and load adapters from built-in and user directories.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::debug;

use crate::adapter::Adapter;

// Built-in adapters embedded at compile time.
const BUILTIN_ADAPTERS: &[(&str, &str)] = &[
    ("hackernews", include_str!("../adapters/hackernews.yaml")),
    ("github-trending", include_str!("../adapters/github_trending.yaml")),
    ("arxiv", include_str!("../adapters/arxiv.yaml")),
    ("wikipedia", include_str!("../adapters/wikipedia.yaml")),
    ("bilibili", include_str!("../adapters/bilibili.yaml")),
    ("v2ex", include_str!("../adapters/v2ex.yaml")),
    ("douban", include_str!("../adapters/douban.yaml")),
    ("zhihu", include_str!("../adapters/zhihu.yaml")),
    ("weibo", include_str!("../adapters/weibo.yaml")),
    ("reddit", include_str!("../adapters/reddit.yaml")),
    ("stackoverflow", include_str!("../adapters/stackoverflow.yaml")),
    ("github", include_str!("../adapters/github.yaml")),
    ("devto", include_str!("../adapters/devto.yaml")),
    ("lobsters", include_str!("../adapters/lobsters.yaml")),
    ("huggingface", include_str!("../adapters/huggingface.yaml")),
    ("steam", include_str!("../adapters/steam.yaml")),
    ("36kr", include_str!("../adapters/36kr.yaml")),
    ("medium", include_str!("../adapters/medium.yaml")),
    ("bbc", include_str!("../adapters/bbc.yaml")),
    ("producthunt", include_str!("../adapters/producthunt.yaml")),
    ("xueqiu", include_str!("../adapters/xueqiu.yaml")),
    ("bloomberg", include_str!("../adapters/bloomberg.yaml")),
    ("binance", include_str!("../adapters/binance.yaml")),
    ("sinafinance", include_str!("../adapters/sinafinance.yaml")),
    ("bluesky", include_str!("../adapters/bluesky.yaml")),
    ("google", include_str!("../adapters/google.yaml")),
    ("gitee", include_str!("../adapters/gitee.yaml")),
    ("imdb", include_str!("../adapters/imdb.yaml")),
    ("tieba", include_str!("../adapters/tieba.yaml")),
    ("dictionary", include_str!("../adapters/dictionary.yaml")),
    ("weread", include_str!("../adapters/weread.yaml")),
    ("youtube", include_str!("../adapters/youtube.yaml")),
    ("xiaohongshu", include_str!("../adapters/xiaohongshu.yaml")),
    ("linkedin", include_str!("../adapters/linkedin.yaml")),
    ("cursor", include_str!("../adapters/cursor.yaml")),
    ("chatgpt", include_str!("../adapters/chatgpt.yaml")),
    ("douyin", include_str!("../adapters/douyin.yaml")),
    ("jd", include_str!("../adapters/jd.yaml")),
    ("taobao", include_str!("../adapters/taobao.yaml")),
    ("kuaishou", include_str!("../adapters/kuaishou.yaml")),
    ("twitter", include_str!("../adapters/twitter.yaml")),
    ("instagram", include_str!("../adapters/instagram.yaml")),
    ("tiktok", include_str!("../adapters/tiktok.yaml")),
    ("facebook", include_str!("../adapters/facebook.yaml")),
    ("jike", include_str!("../adapters/jike.yaml")),
    ("hupu", include_str!("../adapters/hupu.yaml")),
    ("zsxq", include_str!("../adapters/zsxq.yaml")),
    ("substack", include_str!("../adapters/substack.yaml")),
    ("apple-podcasts", include_str!("../adapters/apple-podcasts.yaml")),
    ("yahoo-finance", include_str!("../adapters/yahoo-finance.yaml")),
    ("reuters", include_str!("../adapters/reuters.yaml")),
    ("xiaoyuzhou", include_str!("../adapters/xiaoyuzhou.yaml")),
    ("xianyu", include_str!("../adapters/xianyu.yaml")),
    ("smzdm", include_str!("../adapters/smzdm.yaml")),
    ("pixiv", include_str!("../adapters/pixiv.yaml")),
    ("lesswrong", include_str!("../adapters/lesswrong.yaml")),
    ("ctrip", include_str!("../adapters/ctrip.yaml")),
    ("1688", include_str!("../adapters/1688.yaml")),
    ("amazon", include_str!("../adapters/amazon.yaml")),
    ("coupang", include_str!("../adapters/coupang.yaml")),
    ("boss", include_str!("../adapters/boss.yaml")),
    ("maimai", include_str!("../adapters/maimai.yaml")),
    ("eastmoney", include_str!("../adapters/eastmoney.yaml")),
    ("ths", include_str!("../adapters/ths.yaml")),
    ("tdx", include_str!("../adapters/tdx.yaml")),
    ("barchart", include_str!("../adapters/barchart.yaml")),
    ("doubao", include_str!("../adapters/doubao.yaml")),
    ("gemini", include_str!("../adapters/gemini.yaml")),
    ("grok", include_str!("../adapters/grok.yaml")),
    ("yuanbao", include_str!("../adapters/yuanbao.yaml")),
    ("discord-app", include_str!("../adapters/discord-app.yaml")),
    ("notion", include_str!("../adapters/notion.yaml")),
    ("chatwise", include_str!("../adapters/chatwise.yaml")),
    ("codex", include_str!("../adapters/codex.yaml")),
    ("chatgpt-app", include_str!("../adapters/chatgpt-app.yaml")),
    ("antigravity", include_str!("../adapters/antigravity.yaml")),
    ("mubu", include_str!("../adapters/mubu.yaml")),
    ("notebooklm", include_str!("../adapters/notebooklm.yaml")),
    ("ones", include_str!("../adapters/ones.yaml")),
    ("quark", include_str!("../adapters/quark.yaml")),
    ("xiaoe", include_str!("../adapters/xiaoe.yaml")),
    ("weixin", include_str!("../adapters/weixin.yaml")),
    ("cnki", include_str!("../adapters/cnki.yaml")),
    ("linux-do", include_str!("../adapters/linux-do.yaml")),
    ("sinablog", include_str!("../adapters/sinablog.yaml")),
    ("band", include_str!("../adapters/band.yaml")),
    ("ke", include_str!("../adapters/ke.yaml")),
];

/// Adapter registry holding all available adapters.
#[derive(Debug)]
pub struct Registry {
    adapters: HashMap<String, Adapter>,
}

impl Registry {
    /// Load all adapters: built-in + user directory.
    ///
    /// User adapters from `~/.anycli/adapters/` override built-in ones
    /// with the same name.
    pub fn load() -> Result<Self> {
        let mut adapters = HashMap::new();

        // Load built-in adapters.
        for (name, yaml) in BUILTIN_ADAPTERS {
            match serde_yaml_ng::from_str::<Adapter>(yaml) {
                Ok(adapter) => { adapters.insert(name.to_string(), adapter); }
                Err(e) => { debug!(name, error = %e, "failed to parse built-in adapter"); }
            }
        }

        // Load user adapters (override built-in).
        if let Some(user_dir) = user_adapter_dir() {
            if user_dir.is_dir() {
                load_dir(&user_dir, &mut adapters)?;
            }
        }

        Ok(Self { adapters })
    }

    /// Load adapters from a specific directory (in addition to built-in).
    pub fn load_with_dir(extra_dir: &Path) -> Result<Self> {
        let mut registry = Self::load()?;
        if extra_dir.is_dir() {
            load_dir(extra_dir, &mut registry.adapters)?;
        }
        Ok(registry)
    }

    /// Find an adapter by name.
    pub fn find(&self, name: &str) -> Result<&Adapter> {
        self.adapters.get(name).with_context(|| {
            let available: Vec<&str> = self.adapters.keys().map(|s| s.as_str()).collect();
            format!("adapter `{name}` not found. available: {}", available.join(", "))
        })
    }

    /// List all available adapter names.
    pub fn list(&self) -> Vec<&Adapter> {
        let mut adapters: Vec<&Adapter> = self.adapters.values().collect();
        adapters.sort_by_key(|a| &a.name);
        adapters
    }

    /// Number of loaded adapters.
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Whether the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

/// Load all `.yaml` / `.yml` files from a directory into the adapter map.
fn load_dir(dir: &Path, adapters: &mut HashMap<String, Adapter>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read adapter dir: {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext != "yaml" && ext != "yml" {
            continue;
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;

        match serde_yaml_ng::from_str::<Adapter>(&content) {
            Ok(adapter) => {
                debug!(name = adapter.name, path = %path.display(), "loaded user adapter");
                adapters.insert(adapter.name.clone(), adapter);
            }
            Err(e) => {
                debug!(path = %path.display(), error = %e, "skipping invalid adapter");
            }
        }
    }

    Ok(())
}

/// Default user adapter directory: `~/.anycli/adapters/`.
fn user_adapter_dir() -> Option<PathBuf> {
    dirs_next::home_dir().map(|h| h.join(".anycli").join("adapters"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_adapters_parse() {
        let registry = Registry::load().expect("load");
        assert!(registry.len() >= 5, "expected at least 5 built-in adapters");
        assert!(registry.find("hackernews").is_ok());
        assert!(registry.find("wikipedia").is_ok());
        assert!(registry.find("bilibili").is_ok());
        assert!(registry.find("arxiv").is_ok());
        assert!(registry.find("github-trending").is_ok());
    }

    #[test]
    fn unknown_adapter_errors() {
        let registry = Registry::load().expect("load");
        assert!(registry.find("nonexistent").is_err());
    }
}
