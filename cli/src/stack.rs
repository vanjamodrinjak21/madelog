//! The technologies a repository is built with, read from its manifests.
//!
//! Only what a manifest names is reported: a dependency in a `package.json`,
//! a `require` in `go.mod`, a dependency in `Cargo.toml`. Nothing is inferred
//! from file extensions or guessed from names, so the printed sheet never
//! claims a technology the repository does not declare.

use std::collections::BTreeSet;
use std::path::Path;

/// At most this many entries; a stack line is a glance, not an inventory.
pub const MAX: usize = 8;

/// `(manifest key, label)`, in the order the label is shown. Languages first,
/// then frameworks, data, infrastructure and testing.
const NPM: &[(&str, &str)] = &[
    ("typescript", "TypeScript"),
    ("next", "Next.js"),
    ("react-native", "React Native"),
    ("react-native-web", "React Native Web"),
    ("react", "React"),
    ("vue", "Vue"),
    ("svelte", "Svelte"),
    ("astro", "Astro"),
    ("express", "Express"),
    ("hono", "Hono"),
    ("tailwindcss", "Tailwind CSS"),
    ("drizzle-orm", "Drizzle"),
    ("prisma", "Prisma"),
    ("@prisma/client", "Prisma"),
    ("better-auth", "Better Auth"),
    ("three", "Three.js"),
    ("gsap", "GSAP"),
    ("vitest", "Vitest"),
    ("@playwright/test", "Playwright"),
];

const GO: &[(&str, &str)] = &[
    ("github.com/jackc/pgx", "Postgres"),
    ("github.com/lib/pq", "Postgres"),
    ("github.com/go-chi/chi", "chi"),
    ("github.com/gin-gonic/gin", "Gin"),
    ("github.com/riverqueue/river", "River"),
    ("github.com/davidbyttow/govips", "libvips"),
    ("github.com/aws/aws-sdk-go-v2", "S3"),
    ("github.com/minio/minio-go", "S3"),
];

const CARGO: &[(&str, &str)] = &[
    ("tokio", "Tokio"),
    ("axum", "Axum"),
    ("clap", "clap"),
    ("rusqlite", "SQLite"),
    ("sqlx", "SQLx"),
    ("git2", "libgit2"),
];

/// The stack of the repository at `root`.
pub fn detect(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut push = |label: &str| {
        if !found.iter().any(|l| l == label) {
            found.push(label.to_string());
        }
    };

    // Languages that a manifest proves.
    let npm = npm_deps(root);
    let go = std::fs::read_to_string(root.join("go.mod")).ok();
    let cargo = cargo_manifests(root);
    if go.is_some() {
        push("Go");
    }
    if !cargo.is_empty() {
        push("Rust");
    }
    for (key, label) in NPM {
        if npm.contains(*key) {
            push(label);
        }
    }
    if let Some(go) = &go {
        for (module, label) in GO {
            if go.lines().any(|l| {
                l.trim_start().starts_with(module)
                    || l.contains(&format!("\t{module}"))
                    || l.contains(&format!(" {module}"))
            }) {
                push(label);
            }
        }
    }
    for (krate, label) in CARGO {
        if cargo.iter().any(|m| has_cargo_dep(m, krate)) {
            push(label);
        }
    }
    found.truncate(MAX);
    found
}

/// Every dependency name in the root `package.json` and in the workspace
/// packages one level under `apps/` and `packages/`.
fn npm_deps(root: &Path) -> BTreeSet<String> {
    let mut manifests = vec![root.join("package.json")];
    for group in ["apps", "packages"] {
        if let Ok(entries) = std::fs::read_dir(root.join(group)) {
            for entry in entries.flatten() {
                manifests.push(entry.path().join("package.json"));
            }
        }
    }
    let mut out = BTreeSet::new();
    for path in manifests {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        for field in ["dependencies", "devDependencies", "peerDependencies"] {
            if let Some(map) = json.get(field).and_then(|v| v.as_object()) {
                out.extend(map.keys().cloned());
            }
        }
    }
    out
}

/// The text of the root `Cargo.toml` and of any one directory below it.
fn cargo_manifests(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(raw) = std::fs::read_to_string(root.join("Cargo.toml")) {
        out.push(raw);
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if let Ok(raw) = std::fs::read_to_string(entry.path().join("Cargo.toml")) {
                out.push(raw);
            }
        }
    }
    out
}

fn has_cargo_dep(manifest: &str, krate: &str) -> bool {
    manifest.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with(&format!("{krate} ")) || line.starts_with(&format!("{krate}="))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_declared_dependencies_make_the_stack() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("package.json"),
            r#"{"devDependencies":{"typescript":"^5","vitest":"^3"}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("apps/web")).unwrap();
        std::fs::write(
            root.join("apps/web/package.json"),
            r#"{"dependencies":{"next":"16","react":"19","drizzle-orm":"1","left-pad":"1"}}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("go.mod"),
            "module x\n\ngo 1.25\n\nrequire (\n\tgithub.com/jackc/pgx/v5 v5.7.0\n\tgithub.com/riverqueue/river v0.20.0\n)\n",
        )
        .unwrap();
        assert_eq!(
            detect(root),
            vec![
                "Go",
                "TypeScript",
                "Next.js",
                "React",
                "Drizzle",
                "Vitest",
                "Postgres",
                "River"
            ]
        );
    }

    #[test]
    fn an_empty_directory_has_no_stack() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(detect(dir.path()).is_empty());
    }

    #[test]
    fn a_rust_crate_in_a_subdirectory_counts() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("cli")).unwrap();
        std::fs::write(
            dir.path().join("cli/Cargo.toml"),
            "[dependencies]\nclap = { version = \"4\" }\nrusqlite = \"0.32\"\n",
        )
        .unwrap();
        assert_eq!(detect(dir.path()), vec!["Rust", "clap", "SQLite"]);
    }
}
