//! Puts the git commit into the binary, for `--version` and the title bar's
//! new-build notice.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // Twig has no grammar crate: its generated parser is vendored.
    println!("cargo:rerun-if-changed=vendor/tree-sitter-twig/src/parser.c");
    cc::Build::new()
        .include("vendor/tree-sitter-twig/src")
        .file("vendor/tree-sitter-twig/src/parser.c")
        .warnings(false)
        .compile("tree-sitter-twig");
    println!("cargo:rerun-if-changed=assets/icon/ideae.rc");
    println!("cargo:rerun-if-changed=assets/icon/ideae.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Without a resource compiler the program is built with no icon.
        let _ = embed_resource::compile("assets/icon/ideae.rc", embed_resource::NONE).manifest_optional();
    }
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    println!("cargo:rustc-env=IDEAE_COMMIT={commit}");
    // A new commit or checkout changes these. Only those there: cargo runs
    // this again every time for a path that is not, and builds the crate
    // again with it (CI's shallow checkout has no packed-refs).
    let watch = |path: String| {
        if std::path::Path::new(&path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    };
    if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
        watch(format!("{dir}/HEAD"));
    }
    if let Some(dir) = git(&["rev-parse", "--git-common-dir"]) {
        watch(format!("{dir}/refs/heads"));
        watch(format!("{dir}/packed-refs"));
    }
}
