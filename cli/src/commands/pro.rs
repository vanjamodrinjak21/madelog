//! `madelog pro <license key>` — put this machine on the Pro plan.
//!
//! The key comes from the LemonSqueezy receipt. The server activates it for
//! this device (one activation per machine, the limit is LemonSqueezy's) and
//! from then on `madelog publish` also gives you the PDF address.

use anyhow::{Context, Result};

use crate::config::Config;
use crate::keys::DeviceKey;
use crate::publish::{self, Client};
use crate::Ctx;

/// The bytes `POST /v1/devices/{id}/plan` verifies.
pub fn plan_message(device_id: &str, license_key: &str) -> String {
    format!("plan\n{device_id}\n{license_key}")
}

pub fn run(ctx: &mut Ctx<'_>, license_key: &str, server: Option<&str>) -> Result<()> {
    let key_text = license_key.trim();
    if key_text.is_empty() {
        anyhow::bail!("the license key is empty. It is on your LemonSqueezy receipt.");
    }

    let madelog_dir = ctx.madelog_dir()?;
    let config_path = madelog_dir.join("config.toml");
    let mut config = Config::load(&config_path)?;
    let base = match server {
        Some(url) => publish::normalise_base(url),
        None => publish::normalise_base(&config.server.url),
    };
    let client = Client::new(&base);

    let key = DeviceKey::load_or_create()?;
    let device_id = match config
        .server
        .device_id
        .clone()
        .filter(|s| !s.trim().is_empty())
    {
        Some(id) => id,
        None => client.register_device(&key.public_key_hex())?,
    };
    let signature = key.sign_hex(plan_message(&device_id, key_text).as_bytes());

    let plan = match client.activate_plan(&device_id, key_text, &signature) {
        Err(publish::ApiError::UnknownDevice) => {
            // A device_id copied from another machine, or a server that was
            // reset. Register afresh and try once more.
            let device_id = client.register_device(&key.public_key_hex())?;
            let signature = key.sign_hex(plan_message(&device_id, key_text).as_bytes());
            config.server.device_id = Some(device_id.clone());
            client
                .activate_plan(&device_id, key_text, &signature)
                .context("activating the license")?
        }
        other => other.context("activating the license")?,
    };

    if config.server.device_id.as_deref() != Some(plan.device_id.as_str()) {
        config.server.device_id = Some(plan.device_id.clone());
    }
    config.save(&config_path)?;

    match plan.plan_until.as_deref().filter(|s| !s.is_empty()) {
        Some(until) => ctx.say(&format!(
            "this machine is on madelog {} until {}",
            plan.plan, until
        ))?,
        None => ctx.say(&format!("this machine is on madelog {}", plan.plan))?,
    }
    if plan.plan == "pro" {
        ctx.say("  madelog publish   now also prints the PDF address of your page")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_message_is_the_documented_byte_string() {
        assert_eq!(
            plan_message("6ba7b810-9dad-11d1-80b4-00c04fd430c8", "KEY-1"),
            "plan\n6ba7b810-9dad-11d1-80b4-00c04fd430c8\nKEY-1"
        );
    }
}
