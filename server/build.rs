use std::{env, path::PathBuf, process::Command};

fn main() {
    // Only watch vendor/yt-dlp when it's embedded: cargo treats a missing watched file as changed,
    // which would rerun this script (and the Go build below) and relink the server on every build.
    if env::var("CARGO_FEATURE_EMBED_YTDLP").is_ok() {
        println!("cargo:rerun-if-changed=vendor/yt-dlp");
    }
    for f in ["tsnet-ffi/main.go", "tsnet-ffi/go.mod", "tsnet-ffi/go.sum"] {
        println!("cargo:rerun-if-changed={f}");
    }
    println!("cargo:rerun-if-env-changed=YTP_GO");

    git_version();

    if env::var("CARGO_FEATURE_EMBEDDED_TAILSCALE").is_ok() {
        build_tsnet();
    }

    if env::var("CARGO_FEATURE_EMBED_YTDLP").is_ok() && !std::path::Path::new("vendor/yt-dlp").exists() {
        panic!("feature `embed-ytdlp` requires server/vendor/yt-dlp (run scripts/fetch-ytdlp.sh)");
    }
}

/// The commit the server is built from, for the version shown in Settings: `YTP_GIT_COMMIT`
/// (full hash), `YTP_GIT_DIRTY` ("1" with uncommitted changes) and `YTP_BUILT_AT` (unix time).
/// Without git (or a repo git won't read) they're left unset; a build without a checkout can pass
/// `YTP_GIT_COMMIT` in the environment instead.
fn git_version() {
    println!("cargo:rerun-if-env-changed=YTP_GIT_COMMIT");
    // `safe.directory`: the repo may sit on a network share owned by another user, which git
    // otherwise refuses ("dubious ownership").
    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(["-c", "safe.directory=*"]).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    let from_env = env::var("YTP_GIT_COMMIT").ok().filter(|c| !c.trim().is_empty());
    let commit = from_env.clone().or_else(|| git(&["rev-parse", "HEAD"]).filter(|c| c.len() >= 7));
    if let Some(c) = &commit {
        println!("cargo:rustc-env=YTP_GIT_COMMIT={}", c.trim());
        let dirty = from_env.is_none() && git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
        println!("cargo:rustc-env=YTP_GIT_DIRTY={}", if dirty { "1" } else { "0" });
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    println!("cargo:rustc-env=YTP_BUILT_AT={now}");

    // Rebuild when HEAD moves (checkout, commit) or the index changes (staging, which is also when
    // the dirty state usually changes). Only files that exist: cargo treats a missing one as
    // always changed.
    let Some(git_dir) = git(&["rev-parse", "--absolute-git-dir"]).map(PathBuf::from) else { return };
    let mut watch = vec![git_dir.join("HEAD"), git_dir.join("index"), git_dir.join("packed-refs")];
    if let Ok(head) = std::fs::read_to_string(git_dir.join("HEAD")) {
        if let Some(r) = head.trim().strip_prefix("ref: ") {
            watch.push(git_dir.join(r));
        }
    }
    for f in watch.iter().filter(|f| f.is_file()) {
        println!("cargo:rerun-if-changed={}", f.display());
    }
}

/// Compile server/tsnet-ffi (a small Go wrapper around tailscale.com/tsnet) into a static
/// library and link it in, so the server can join the tailnet without Tailscale installed.
fn build_tsnet() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let go = env::var("YTP_GO").unwrap_or_else(|_| find_go());
    let lib = out.join("libytpts.a");

    let status = Command::new(&go)
        .args(["build", "-buildmode=c-archive", "-trimpath", "-ldflags=-s -w", "-o"])
        .arg(&lib)
        .arg(".")
        .current_dir("tsnet-ffi")
        .env("CGO_ENABLED", "1")
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("building the embedded Tailscale node (server/tsnet-ffi) failed: {s}"),
        Err(e) => panic!(
            "\n\nThe embedded Tailscale node (on by default) is built with Go, but `{go}` couldn't be run: {e}\n\
             Install Go (https://go.dev/dl/) or point YTP_GO at it, or build without it:\n\
             \tcargo build --no-default-features\n\n"
        ),
    }

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=ytpts");
    match env::var("CARGO_CFG_TARGET_OS").unwrap_or_default().as_str() {
        "linux" => {
            println!("cargo:rustc-link-lib=dylib=pthread");
            println!("cargo:rustc-link-lib=dylib=resolv");
        }
        "macos" => {
            println!("cargo:rustc-link-lib=framework=CoreFoundation");
            println!("cargo:rustc-link-lib=framework=Security");
            println!("cargo:rustc-link-lib=framework=SystemConfiguration");
            println!("cargo:rustc-link-lib=resolv");
        }
        _ => {}
    }
}

/// `go` from PATH, or from the usual install locations (a fresh shell often lacks them on PATH).
fn find_go() -> String {
    let on_path = env::var_os("PATH")
        .map(|p| env::split_paths(&p).any(|d| d.join("go").is_file()))
        .unwrap_or(false);
    if on_path {
        return "go".into();
    }
    let home = env::var("HOME").unwrap_or_default();
    [format!("{home}/.local/go/bin/go"), "/usr/local/go/bin/go".into(), format!("{home}/go/bin/go"), "/usr/lib/go/bin/go".into()]
        .into_iter()
        .find(|p| std::path::Path::new(p).is_file())
        .unwrap_or_else(|| "go".into())
}
