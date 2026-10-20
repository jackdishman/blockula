fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    // Embed the git commit (if built from a checkout) for /api/status and /metrics.
    if let Some(sha) = git(&["rev-parse", "--short=12", "HEAD"]) {
        println!("cargo:rustc-env=BLOCKULA_GIT_SHA={sha}");
    }
    // Rerun when HEAD moves. Ask git where HEAD lives: in a worktree `.git` is a file, and a
    // path that doesn't exist makes cargo rerun this script, and rebuild the crate, every time.
    if let Some(head) = git(&["rev-parse", "--path-format=absolute", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    println!("cargo:rerun-if-env-changed=BLOCKULA_GIT_SHA");
}
