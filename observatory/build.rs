//! Builds the Cosmos hero (`cosmos/cosmos.rs`) to WebAssembly as part of `cargo build`.
//!
//! The module is one `#![no_std]` file with no crates, so this calls `rustc` directly
//! (`--target wasm32-unknown-unknown --crate-type cdylib`) instead of a nested `cargo`
//! (which would fight the outer build over the target-dir lock and the jobserver).
//!
//! Which rustc: the first candidate whose sysroot has the wasm32 target installed:
//! `$OBSERVATORY_WASM_RUSTC`, `$RUSTC` (the one building this crate), `rustup which
//! rustc`, then every `~/.rustup/toolchains/*/bin/rustc`. This matters on machines where
//! cargo comes from Homebrew (no wasm32 std) while rustup has the target.
//!
//! No usable rustc: fall back to the checked-in `cosmos/cosmos.wasm` and warn. Refresh
//! that copy with `OBSERVATORY_REFRESH_WASM=1 cargo build -p observatory`.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const TARGET: &str = "wasm32-unknown-unknown";

fn sysroot(rustc: &Path) -> Option<PathBuf> {
    let out = Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()))
}

fn has_wasm_target(root: &Path) -> bool {
    fs::read_dir(root.join("lib/rustlib").join(TARGET).join("lib"))
        .map(|d| {
            d.flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("libcore-"))
        })
        .unwrap_or(false)
}

fn candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = vec![];
    for var in ["OBSERVATORY_WASM_RUSTC", "RUSTC"] {
        if let Ok(v) = env::var(var) {
            out.push(v.into());
        }
    }
    if let Ok(o) = Command::new("rustup").args(["which", "rustc"]).output()
        && o.status.success()
    {
        out.push(String::from_utf8_lossy(&o.stdout).trim().into());
    }
    let home = env::var("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|_| env::var("HOME").map(|h| Path::new(&h).join(".rustup")));
    if let Ok(home) = home
        && let Ok(dir) = fs::read_dir(home.join("toolchains"))
    {
        let mut tcs: Vec<PathBuf> = dir.flatten().map(|e| e.path().join("bin/rustc")).collect();
        tcs.sort(); // "stable-…" sorts after "nightly-…"; either is fine for this module
        tcs.reverse();
        out.extend(tcs);
    }
    out
}

fn compile(rustc: &Path, root: &Path, src: &Path, out: &Path) -> Result<(), String> {
    let mut cmd = Command::new(rustc);
    cmd.args([
        "--edition",
        "2021",
        "--crate-type",
        "cdylib",
        "--target",
        TARGET,
    ])
    .args(["-C", "opt-level=3", "-C", "panic=abort", "-C", "lto"])
    .args(["-C", "target-feature=+simd128", "-C", "strip=symbols"])
    .args(["--crate-name", "cosmos", "-o"])
    .arg(out)
    .arg(src)
    // the outer build's flags are for the host, not for this module
    .env_remove("CARGO_ENCODED_RUSTFLAGS")
    .env_remove("RUSTFLAGS")
    // some rustup builds ship a rust-lld that cannot find libLLVM.dylib on macOS
    .env("DYLD_FALLBACK_LIBRARY_PATH", root.join("lib"));
    let o = cmd.output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).into_owned())
    }
}

fn main() {
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = dir.join("cosmos/cosmos.rs");
    let prebuilt = dir.join("cosmos/cosmos.wasm");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("cosmos.wasm");
    println!("cargo:rerun-if-changed=cosmos/cosmos.rs");
    println!("cargo:rerun-if-env-changed=OBSERVATORY_WASM_RUSTC");
    println!("cargo:rerun-if-env-changed=OBSERVATORY_REFRESH_WASM");

    let mut errors = vec![];
    let mut built = false;
    let mut seen = vec![];
    for rustc in candidates() {
        let Some(root) = sysroot(&rustc) else {
            continue;
        };
        if seen.contains(&root) || !has_wasm_target(&root) {
            seen.push(root);
            continue;
        }
        seen.push(root.clone());
        match compile(&rustc, &root, &src, &out) {
            Ok(()) => {
                built = true;
                break;
            }
            Err(e) => errors.push(format!(
                "{}: {}",
                rustc.display(),
                e.lines().take(6).collect::<Vec<_>>().join(" | ")
            )),
        }
    }
    if built {
        if env::var_os("OBSERVATORY_REFRESH_WASM").is_some() {
            fs::copy(&out, &prebuilt).expect("refresh cosmos/cosmos.wasm");
        }
        return;
    }
    println!(
        "cargo:warning=observatory: no rustc with the {TARGET} target could build cosmos.rs; using the checked-in cosmos/cosmos.wasm (install with `rustup target add {TARGET}`)"
    );
    for e in errors {
        println!("cargo:warning=observatory: {e}");
    }
    fs::copy(&prebuilt, &out).expect("cosmos/cosmos.wasm is missing and cannot be built");
}
