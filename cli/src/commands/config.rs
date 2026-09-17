//! `pop config get|set|list`.

use anyhow::Result;

use crate::cli::ConfigCommand;
use crate::config::{Config, KEYS};
use crate::Ctx;

pub fn run(ctx: &mut Ctx<'_>, command: ConfigCommand) -> Result<()> {
    let path = ctx.pop_dir()?.join("config.toml");
    let mut config = Config::load(&path)?;

    match command {
        ConfigCommand::Get { key } => {
            let value = config.get(&key)?;
            ctx.say(&value)?;
        }
        ConfigCommand::Set { key, value } => {
            config.set(&key, &value)?;
            config.save(&path)?;
            ctx.say(&format!("{key} = {}", config.get(&key)?.replace('\n', ", ")))?;
        }
        ConfigCommand::List => {
            for key in KEYS {
                ctx.say(&format!("{key} = {}", config.get(key)?.replace('\n', ", ")))?;
            }
        }
    }
    Ok(())
}
