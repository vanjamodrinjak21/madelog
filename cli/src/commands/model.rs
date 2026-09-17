//! `pop model pull` — download the embedded model.
//!
//! Out of scope for v1: pop runs on Ollama or on the heuristic summariser.

use anyhow::Result;

use crate::{Ctx, ExitError};

pub fn pull(_ctx: &mut Ctx<'_>) -> Result<()> {
    Err(ExitError::unavailable("pop model pull"))
}
