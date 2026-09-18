//! Project page generator (task E fills this in; task G consumes it).
//!
//! Contract shared by both tasks — keep these signatures stable:
//! - [`PageContext`] mirrors the "Shared page context" JSON in the plan.
//! - [`build_context`] derives it from the event log and config.
//! - [`render`] renders the Tera template to a single self-contained HTML string.
//! - [`render_local`] writes `.pop/site/index.html` and returns its path.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Server-side publication facts shown in the badge; `None` for local renders.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Published {
    pub slug: String,
    pub url: String,
    pub received_at: String,
    pub verify_url: String,
}

/// Everything the template needs. Task E replaces this placeholder with the
/// full shape from the plan; task G only ever passes it through.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PageContext {
    pub project_name: String,
    pub root: String,
    pub published: Option<Published>,
}

/// Build the page context from the repo's `.pop` log.
pub fn build_context(root: &Path, published: Option<Published>) -> Result<PageContext> {
    let store = crate::store::Store::open(root)?;
    let chain_root = store.root()?;
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "project".to_string());
    Ok(PageContext {
        project_name: name,
        root: chain_root,
        published,
    })
}

/// Render the page to HTML.
pub fn render(ctx: &PageContext) -> Result<String> {
    Ok(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{}</title></head><body><h1>{}</h1><p>root {}</p></body></html>\n",
        ctx.project_name, ctx.project_name, ctx.root
    ))
}

/// Render and write `.pop/site/index.html`, returning its path.
pub fn render_local(root: &Path, published: Option<Published>) -> Result<PathBuf> {
    let ctx = build_context(root, published)?;
    let html = render(&ctx)?;
    let dir = root.join(".pop").join("site");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("index.html");
    std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}
