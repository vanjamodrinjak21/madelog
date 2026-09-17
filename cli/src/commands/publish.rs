//! `pop publish` — render the project page, preview it, sign the root and send it.
//!
//! Task E renders the page and task G adds the preview, signing and upload.

use anyhow::Result;

use crate::{Ctx, ExitError};

pub struct Options {
    pub local: bool,
    pub server: Option<String>,
    pub yes: bool,
}

pub fn run(_ctx: &mut Ctx<'_>, _opts: Options) -> Result<()> {
    Err(ExitError::not_implemented("pop publish"))
}
