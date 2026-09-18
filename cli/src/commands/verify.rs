//! `madelog verify` — recompute the chain from the first event.

use anyhow::Result;

use crate::chain::Verification;
use crate::{Ctx, ExitError};

pub fn run(ctx: &mut Ctx<'_>) -> Result<()> {
    match ctx.open_store()?.verify()? {
        Verification::Ok { root, count } => {
            ctx.say("chain ok")?;
            ctx.say(&format!("{count} events, root {root}"))?;
            Ok(())
        }
        Verification::Broken { seq, reason, count } => Err(ExitError::new(
            1,
            format!("chain broken at seq {seq}: {reason} ({count} events read)"),
        )
        .into()),
    }
}
