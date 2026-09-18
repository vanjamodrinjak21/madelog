//! `madelog ask` — question answering over the project history.
//!
//! Out of scope for v1; it needs the embedded model and an index.

use anyhow::Result;

use crate::{Ctx, ExitError};

pub fn run(_ctx: &mut Ctx<'_>, _question: &str) -> Result<()> {
    Err(ExitError::unavailable("madelog ask"))
}
