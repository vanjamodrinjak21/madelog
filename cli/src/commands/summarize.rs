//! `pop summarize` and `pop summary edit` — weekly summaries.
//!
//! Task D adds the Ollama client and the heuristic fallback behind these.

use anyhow::Result;

use crate::{Ctx, ExitError};

pub fn run(_ctx: &mut Ctx<'_>, _week: Option<u32>) -> Result<()> {
    Err(ExitError::not_implemented("pop summarize"))
}

pub fn edit(_ctx: &mut Ctx<'_>, _event_id: &str) -> Result<()> {
    Err(ExitError::not_implemented("pop summary edit"))
}
