//! `pop analyze` — find patterns in the log and record them as events.
//!
//! Task C implements the pattern and moment detection behind this.

use anyhow::Result;

use crate::{Ctx, ExitError};

pub fn run(_ctx: &mut Ctx<'_>) -> Result<()> {
    Err(ExitError::not_implemented("pop analyze"))
}
