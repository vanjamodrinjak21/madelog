//! `madelog publish` and `madelog unpublish` — the only two commands that talk to a
//! network, and the only moment anything leaves this machine (spec §7).
//!
//! The order is fixed, and it matters:
//!
//! 1. refuse outside a repository madelog knows,
//! 2. bring patterns and summaries up to date, so the page is about today,
//! 3. show the preview and let it change the log (hide, edit),
//! 4. render `.madelog/site/index.html` — always, so `--local` has produced
//!    something even when the network is down,
//! 5. sign the chain root and POST the page, then render it once more with the
//!    server's confirmation in the badge and POST that to the same slug, so
//!    the page the company opens carries the badge rather than a draft.
//!
//! Nothing between step 3 and step 5 appends to the log, so the root the page
//! shows is the root that gets signed — both times. The second upload is the
//! only part of this that is allowed to fail quietly: by then the page is
//! already public, and a warning beats a non-zero exit.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::keys::DeviceKey;
use crate::page::{self, Published};
use crate::preview::{self, Outcome, Preview};
use crate::publish::{self, ApiError, PageRequest, MAX_HTML_BYTES};
use crate::store::Store;
use crate::{commands, Ctx, ExitError};

pub struct Options {
    pub local: bool,
    pub server: Option<String>,
    pub yes: bool,
}

pub struct UnpublishOptions {
    pub server: Option<String>,
}

/// `madelog publish [--local] [--server <url>] [--yes]`.
pub fn run(ctx: &mut Ctx<'_>, opts: Options) -> Result<()> {
    let mut screen = preview::default_preview();
    run_with(ctx, opts, screen.as_mut())
}

/// The same command with the preview screen handed in, so tests can walk the
/// confirm and abort paths without a terminal.
pub fn run_with(ctx: &mut Ctx<'_>, opts: Options, screen: &mut dyn Preview) -> Result<()> {
    reject_empty_server(opts.server.as_deref())?;
    let root = project_root(ctx)?;

    refresh(ctx)?;
    if let Some(warning) = preview::thin_warning(count_events(&root)?) {
        ctx.say(&format!("warning: {warning}"))?;
        ctx.blank()?;
    }

    // Step 3. `--yes` and a missing terminal both mean "no preview" (spec §4.5).
    if !opts.yes && screen.is_available() {
        let model = preview::Model::build(&root)?;
        let mut host = preview::LogHost::new(&root);
        let outcome = screen.run(model, &mut host);
        let said = std::mem::take(&mut host.said);
        let outcome = outcome?;
        for line in said {
            ctx.say(&line)?;
        }
        if outcome == Outcome::Abort {
            return Err(ExitError::new(1, "publish aborted").into());
        }
    }

    // Step 4. Always render locally first: a failed upload still leaves a page.
    let path = page::render_local(&root, None)?;
    ctx.say(&format!("page   {}", path.display()))?;
    if opts.local {
        ctx.say("not published. Run `madelog publish` without --local to put it online.")?;
        return Ok(());
    }

    upload(ctx, &root, &path, opts.server.as_deref())
}

/// `madelog unpublish` — take the page down, keep the chain.
pub fn unpublish(ctx: &mut Ctx<'_>, opts: UnpublishOptions) -> Result<()> {
    reject_empty_server(opts.server.as_deref())?;
    let root = project_root(ctx)?;
    let config_path = config_path(&root);
    let mut config = Config::load(&config_path)?;

    let Some(slug) = present(config.server.slug.clone()) else {
        bail!(
            "this repository has no published page. `madelog publish` first, or check \
             `madelog config get server.slug`."
        );
    };

    let client = publish::Client::new(&server_url(&config, opts.server.as_deref())?);
    let key = DeviceKey::load_or_create()?;
    let device_id = match present(config.server.device_id.clone()) {
        Some(id) => id,
        None => client.register_device(&key.public_key_hex())?,
    };

    let signature = key.sign_hex(publish::delete_message(&slug).as_bytes());
    client.delete_page(&slug, &device_id, &signature)?;

    config.server.slug = None;
    config.server.device_id = Some(device_id);
    config.save(&config_path)?;

    ctx.say(&format!(
        "{slug} is gone from {}. The chain is untouched, and `madelog publish` can put it back.",
        client.base()
    ))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// steps
// ---------------------------------------------------------------------------

/// Step 1. `madelog publish` outside a repository madelog has recorded is a mistake,
/// not a thing to guess at.
fn project_root(ctx: &Ctx<'_>) -> Result<PathBuf> {
    let root = crate::gitrepo::Repo::discover(&ctx.cwd)
        .context("`madelog publish` runs inside a git repository")?
        .workdir()?;
    if !root.join(".madelog").join("log.db").exists() {
        bail!(
            "there is no madelog log in {} — run `madelog init` first, then commit something. \
             `madelog publish` has nothing to publish until then.",
            root.display()
        );
    }
    Ok(root)
}

/// Step 2. Patterns and summaries, through the very commands that own them.
fn refresh(ctx: &mut Ctx<'_>) -> Result<()> {
    commands::analyze::run(ctx, false)?;
    ctx.blank()?;
    commands::summarize::run(ctx, None, false)?;
    ctx.blank()
}

/// Step 5.
fn upload(ctx: &mut Ctx<'_>, root: &Path, page_path: &Path, server: Option<&str>) -> Result<()> {
    let config_path = config_path(root);
    let mut config = Config::load(&config_path)?;
    let client = publish::Client::new(&server_url(&config, server)?);

    let html = std::fs::read_to_string(page_path)
        .with_context(|| format!("reading {}", page_path.display()))?;
    check_size(html.len(), page_path)?;

    let facts = Facts::read(root)?;
    let key = DeviceKey::load_or_create()?;
    let public_key = key.public_key_hex();

    let mut device_id = match present(config.server.device_id.clone()) {
        Some(id) => id,
        None => client.register_device(&public_key)?,
    };

    let slug = present(config.server.slug.clone());
    let published_ts = now_rfc3339();
    let signature = key.sign_hex(
        publish::publish_message(&facts.root, &published_ts, slug.as_deref().unwrap_or(""))
            .as_bytes(),
    );

    let mut request = PageRequest {
        device_id: device_id.clone(),
        slug: slug.clone(),
        html,
        root: facts.root.clone(),
        first_ts: facts.first_ts.clone(),
        last_ts: facts.last_ts.clone(),
        published_ts,
        signature,
    };

    let response = match client.publish_page(&request) {
        Ok(response) => response,
        // The server has forgotten this device (a fresh database, or a
        // `device_id` copied from another machine). Register again and retry
        // once; the signature does not cover the device id, so it still holds.
        Err(ApiError::UnknownDevice) => {
            device_id = client.register_device(&public_key)?;
            request.device_id = device_id.clone();
            client.publish_page(&request)?
        }
        Err(other) => return Err(other.into()),
    };

    config.server.device_id = Some(device_id);
    config.server.slug = Some(response.slug.clone());
    config.save(&config_path)?;

    let url = if response.url.trim().is_empty() {
        format!("{}/p/{}", client.base(), response.slug)
    } else {
        response.url.clone()
    };
    let verify_url = if response.verify_url.trim().is_empty() {
        format!("{}/v1/verify/{}", client.base(), response.slug)
    } else {
        response.verify_url.clone()
    };

    // Render once more, now that the badge has something to confirm.
    let path = page::render_local(
        root,
        Some(Published {
            slug: response.slug.clone(),
            url: url.clone(),
            received_at: response.received_at.clone(),
            verify_url: verify_url.clone(),
        }),
    )?;

    // The first upload could only carry "not yet published" — the slug did not
    // exist when it was rendered. Send the confirmed page over the top of it,
    // so what the company opens is the page with the badge, not the draft.
    confirm(ctx, &client, &key, &mut request, &response.slug, &path)?;

    ctx.say(&format!("page   {}", path.display()))?;
    ctx.say(&format!("verify {verify_url}"))?;
    ctx.blank()?;
    ctx.say(&format!("published  {url}"))?;
    ctx.say("Send that link. `madelog unpublish` takes it down again.")?;
    Ok(())
}

/// Step 5b: re-publish the confirmed page to the slug it just got.
///
/// The page is already online at this point, so nothing here is fatal. A
/// failure leaves the visitor looking at the unconfirmed badge, which is worth
/// a warning and another `madelog publish`, not a non-zero exit.
fn confirm(
    ctx: &mut Ctx<'_>,
    client: &publish::Client,
    key: &DeviceKey,
    request: &mut PageRequest,
    slug: &str,
    page_path: &Path,
) -> Result<()> {
    let html = match std::fs::read_to_string(page_path) {
        Ok(html) => html,
        Err(e) => return warn(ctx, &format!("reading {}: {e}", page_path.display())),
    };
    if let Err(e) = check_size(html.len(), page_path) {
        return warn(ctx, &format!("{e:#}"));
    }

    // Same root, same timestamps, now with the slug the server minted — and a
    // signature over exactly those bytes.
    request.html = html;
    request.slug = Some(slug.to_string());
    request.signature = key
        .sign_hex(publish::publish_message(&request.root, &request.published_ts, slug).as_bytes());

    match client.publish_page(request) {
        Ok(_) => Ok(()),
        Err(e) => warn(ctx, &format!("{e}")),
    }
}

/// Something went wrong after the page was already public.
fn warn(ctx: &mut Ctx<'_>, why: &str) -> Result<()> {
    ctx.say(&format!(
        "warning: the page is online, but the confirmed badge could not be sent to it ({why}). \
         Visitors see \"not yet published\" until the next `madelog publish`."
    ))
}

// ---------------------------------------------------------------------------
// small things
// ---------------------------------------------------------------------------

/// What the server is told about the chain, read after every append is done.
struct Facts {
    root: String,
    first_ts: String,
    last_ts: String,
}

impl Facts {
    fn read(root: &Path) -> Result<Facts> {
        let store = Store::open(&root.join(".madelog").join("log.db"))?;
        let events = store.all()?;
        let now = now_rfc3339();
        Ok(Facts {
            root: crate::chain::root(&events),
            first_ts: events
                .first()
                .map(|e| e.ts.clone())
                .unwrap_or_else(|| now.clone()),
            last_ts: events.last().map(|e| e.ts.clone()).unwrap_or(now),
        })
    }
}

/// The server answers 413 above 2 MB. Finding that out after uploading two
/// megabytes is rude, so madelog says it first, locally.
fn check_size(bytes: usize, page_path: &Path) -> Result<()> {
    if bytes > MAX_HTML_BYTES {
        bail!(
            "the page is {} and the server accepts {} at most. It is written at {}; \
             hide a session or two with `madelog hide` and publish again.",
            megabytes(bytes),
            megabytes(MAX_HTML_BYTES),
            page_path.display()
        );
    }
    Ok(())
}

fn count_events(root: &Path) -> Result<usize> {
    Store::open(&root.join(".madelog").join("log.db"))?.count()
}

fn config_path(root: &Path) -> PathBuf {
    root.join(".madelog").join("config.toml")
}

/// `--server` wins over `server.url`; neither is allowed to be empty.
///
/// An empty `--server` is a typo, not a request to fall back to the config:
/// silently publishing somewhere else is worse than refusing.
fn server_url(config: &Config, flag: Option<&str>) -> Result<String> {
    reject_empty_server(flag)?;
    if let Some(flag) = flag {
        return Ok(flag.to_string());
    }
    let chosen = config.server.url.clone();
    if chosen.trim().is_empty() {
        bail!(
            "no server to publish to. Set one with `madelog config set server.url <url>`, \
             pass `--server <url>`, or stay offline with `madelog publish --local`."
        );
    }
    Ok(chosen)
}

/// RFC 3339 in UTC, to the second, exactly as the signature and the body carry it.
/// `--server ""` is a typo — an empty argument, or a shell variable that did
/// not expand. Falling back to the configured server would publish somewhere
/// the person did not name, so this is refused before anything is rendered.
fn reject_empty_server(flag: Option<&str>) -> Result<()> {
    if flag.is_some_and(|s| s.trim().is_empty()) {
        return Err(ExitError::new(
            2,
            "`--server` needs a url, for example `--server https://madelog.app`. \
             Leave the flag off to use `server.url`, or pass `--local` to stay offline.",
        )
        .into());
    }
    Ok(())
}

fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn present(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

fn megabytes(bytes: usize) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flag_beats_the_config_and_an_empty_one_is_refused() {
        let mut config = Config::default();
        config.server.url = "https://madelog.app".into();
        assert_eq!(
            server_url(&config, Some("http://127.0.0.1:9")).unwrap(),
            "http://127.0.0.1:9"
        );
        assert_eq!(server_url(&config, None).unwrap(), "https://madelog.app");

        // An empty `--server` never falls back to the config.
        for empty in ["", "  "] {
            let err = server_url(&config, Some(empty)).unwrap_err();
            let exit = err.downcast_ref::<ExitError>().expect("an ExitError");
            assert_eq!(exit.code, 2, "{err:#}");
            assert!(format!("{err:#}").contains("--server"), "{err:#}");
        }

        config.server.url = String::new();
        let err = server_url(&config, None).unwrap_err();
        assert!(format!("{err:#}").contains("--local"), "{err:#}");
    }

    #[test]
    fn the_published_timestamp_is_rfc3339_utc_to_the_second() {
        let ts = now_rfc3339();
        assert!(ts.ends_with('Z'), "{ts}");
        assert!(!ts.contains('.'), "no fractional seconds: {ts}");
        assert_eq!(ts.len(), 20, "{ts}");
        chrono::DateTime::parse_from_rfc3339(&ts).expect("parses as RFC 3339");
    }

    #[test]
    fn a_page_over_two_megabytes_is_refused_before_it_is_uploaded() {
        let path = Path::new("/tmp/.madelog/site/index.html");
        check_size(MAX_HTML_BYTES, path).expect("exactly 2 MiB is allowed");
        let err = check_size(MAX_HTML_BYTES + 1, path).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("2.0 MB"), "{text}");
        assert!(text.contains("index.html"), "{text}");
        assert!(text.contains("madelog hide"), "{text}");
    }

    #[test]
    fn an_absent_value_and_an_empty_one_are_the_same_thing() {
        assert_eq!(present(Some("x".into())), Some("x".into()));
        assert_eq!(present(Some("  ".into())), None);
        assert_eq!(present(None), None);
    }
}
