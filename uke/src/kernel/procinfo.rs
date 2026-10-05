//! OS process facts the kernel uses to authenticate callers: the peer of a socket,
//! a process's ancestors, and liveness. Identity is derived, never claimed.
use std::os::unix::{io::AsRawFd, net::UnixStream};

/// PID of the process on the other end of a Unix socket.
pub fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let fd = stream.as_raw_fd();
    #[cfg(target_os = "macos")]
    unsafe {
        let mut pid: libc::pid_t = 0;
        let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        let ok = libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        ) == 0;
        (ok && pid > 0).then_some(pid as u32)
    }
    #[cfg(target_os = "linux")]
    unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let ok = libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        ) == 0;
        (ok && cred.pid > 0).then_some(cred.pid as u32)
    }
}

/// (parent pid, command name) of a live process.
pub fn parent(pid: u32) -> Option<(u32, String)> {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let n = libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        );
        if n != size {
            return None;
        }
        let name = |raw: &[libc::c_char]| {
            let bytes: Vec<u8> = raw
                .iter()
                .take_while(|c| **c != 0)
                .map(|c| *c as u8)
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        };
        let long = name(&info.pbi_name);
        Some((
            info.pbi_ppid,
            if long.is_empty() {
                name(&info.pbi_comm)
            } else {
                long
            },
        ))
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        let comm = stat[open + 1..close].to_owned();
        let ppid = stat[close + 2..].split_whitespace().nth(1)?.parse().ok()?;
        Some((ppid, comm))
    }
}

/// Ancestors of `pid` (nearest first), excluding `pid` itself.
pub fn ancestors(pid: u32) -> Vec<(u32, String)> {
    let mut out = vec![];
    let mut current = pid;
    for _ in 0..24 {
        let Some((ppid, _)) = parent(current) else {
            break;
        };
        if ppid <= 1 {
            break;
        }
        let comm = parent(ppid).map(|(_, c)| c).unwrap_or_default();
        out.push((ppid, comm));
        current = ppid;
    }
    out
}

pub fn is_shell(comm: &str) -> bool {
    let base = comm
        .rsplit('/')
        .next()
        .unwrap_or(comm)
        .trim_start_matches('-');
    matches!(
        base,
        "sh" | "bash" | "zsh" | "dash" | "fish" | "env" | "timeout" | "script" | "nice" | "nohup"
    )
}

/// The harness process that ran a hook: the nearest non-shell ancestor.
pub fn harness_process(hook_pid: u32) -> Option<u32> {
    ancestors(hook_pid)
        .into_iter()
        .find(|(_, comm)| !is_shell(comm))
        .map(|(pid, _)| pid)
}

pub fn alive(pid: u32) -> bool {
    match crate::signals::process_exists(i64::from(pid)) {
        Ok(()) => true,
        Err(e) => e.raw_os_error() == Some(libc::EPERM),
    }
}

/// Executable path of a live process.
pub fn path(pid: u32) -> Option<String> {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut buf = vec![0u8; 4096];
        let n = libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as u32,
        );
        if n <= 0 {
            return None;
        }
        buf.truncate(n as usize);
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .ok()
            .map(|p| p.display().to_string())
    }
}

/// Which app a harness process belongs to: `t3` (T3 Code), `codex-app` (the Codex
/// app inside ChatGPT.app), `claude-desktop` (the Code tab of the Claude macOS app),
/// `claude-cli`, `codex-cli`, else `headless`.
pub fn app_of(harness_pid: u32) -> String {
    let chain: Vec<u32> = std::iter::once(harness_pid)
        .chain(ancestors(harness_pid).into_iter().map(|(p, _)| p))
        .collect();
    let paths: Vec<String> = chain.iter().filter_map(|p| path(*p)).collect();
    classify(&paths, std::env::var_os("UNVRS_DRIVEN_PID").is_some()).into()
}

/// `app_of` for a harness that is still running (its executable path is readable);
/// None once it has exited, so a dead PID never relabels a thread.
pub fn live_app(harness_pid: u32) -> Option<String> {
    path(harness_pid).map(|_| app_of(harness_pid))
}

/// `app_of` over executable paths (the harness first, then its ancestors, nearest
/// first). `driven` is true inside a kernel-driven run.
///
/// Claude Desktop runs Claude Code from its own bundle
/// (`~/Library/Application Support/Claude/claude-code/<v>/claude.app/.../claude`) under
/// `/Applications/Claude.app/Contents/{MacOS/Claude,Helpers/disclaimer}`, and sets
/// `CLAUDE_CODE_ENTRYPOINT=claude-desktop` (the kernel cannot read another process's
/// environment, so the paths decide). The Claude.app match is case-sensitive: the
/// inner bundle is lower-case `claude.app`.
pub fn classify(paths: &[String], driven: bool) -> &'static str {
    if paths.iter().any(|p| p.contains("T3 Code")) {
        return "t3";
    }
    if paths
        .iter()
        .any(|p| p.contains("ChatGPT.app") || p.contains("/Codex.app/"))
    {
        return "codex-app";
    }
    let own = paths.first().map(String::as_str).unwrap_or("");
    let base = own.rsplit('/').next().unwrap_or(own);
    if driven {
        return "headless";
    }
    if paths.iter().any(|p| p.contains("/Claude.app/Contents/"))
        || own.contains("/Application Support/Claude/claude-code/")
    {
        return "claude-desktop";
    }
    let terminal = paths.iter().skip(1).any(|p| {
        p.contains("Terminal.app")
            || p.contains("iTerm")
            || p.contains("Ghostty")
            || p.contains("WezTerm")
            || p.contains("Alacritty")
            || p.contains("kitty")
            || p.contains("Warp")
            || p.ends_with("/login")
            || p.ends_with("/tmux")
    });
    match (base.contains("claude"), base.contains("codex"), terminal) {
        (true, _, true) => "claude-cli",
        (_, true, true) => "codex-cli",
        _ => "headless",
    }
}

/// A harness (Claude Code, Codex, T3 Code, the Codex app) among the ancestors: the
/// caller is a model's tool, not the captain's terminal.
pub fn under_harness(pid: u32) -> bool {
    std::iter::once(pid)
        .chain(ancestors(pid).into_iter().map(|(p, _)| p))
        .any(|p| {
            let comm = parent(p).map(|(_, c)| c).unwrap_or_default();
            let path = path(p).unwrap_or_default();
            let base = comm.rsplit('/').next().unwrap_or(&comm).to_owned();
            base == "claude"
                || base == "codex"
                || base.starts_with("claude")
                || path.contains("T3 Code")
                || path.contains("ChatGPT.app")
                || path.contains("/claude-code/")
                || path.contains("/codex/")
        })
}

#[cfg(test)]
mod tests {
    use super::classify;

    fn v(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }

    /// The chain observed on this Mac for a Claude Desktop Code-tab session.
    const DESKTOP: &[&str] = &[
        "/Users/c/Library/Application Support/Claude/claude-code/2.1.284/claude.app/Contents/MacOS/claude",
        "/Applications/Claude.app/Contents/Helpers/disclaimer",
        "/Applications/Claude.app/Contents/MacOS/Claude",
    ];

    #[test]
    fn claude_desktop_threads_are_told_apart() {
        assert_eq!(classify(&v(DESKTOP), false), "claude-desktop");
        // Its bundled claude alone (parent not readable) still identifies it.
        assert_eq!(classify(&v(&DESKTOP[..1]), false), "claude-desktop");
        // A shell in the Code tab's terminal pane runs a CLI `claude` under the app.
        assert_eq!(
            classify(
                &v(&[
                    "/opt/homebrew/bin/claude",
                    "/bin/zsh",
                    DESKTOP[0],
                    DESKTOP[1],
                    DESKTOP[2]
                ]),
                false
            ),
            "claude-desktop"
        );
    }

    #[test]
    fn other_apps_keep_their_names() {
        assert_eq!(
            classify(
                &v(&[
                    "/usr/local/bin/codex",
                    "/Applications/T3 Code.app/Contents/MacOS/T3 Code"
                ]),
                false
            ),
            "t3"
        );
        assert_eq!(
            classify(
                &v(&[
                    "/x/codex",
                    "/Applications/ChatGPT.app/Contents/MacOS/ChatGPT"
                ]),
                false
            ),
            "codex-app"
        );
        assert_eq!(
            classify(
                &v(&[
                    "/opt/homebrew/bin/claude",
                    "/bin/zsh",
                    "/usr/bin/login",
                    "/Applications/Ghostty.app/Contents/MacOS/ghostty"
                ]),
                false
            ),
            "claude-cli"
        );
        assert_eq!(
            classify(
                &v(&[
                    "/opt/homebrew/bin/codex",
                    "/bin/zsh",
                    "/Applications/WezTerm.app/Contents/MacOS/wezterm-gui"
                ]),
                false
            ),
            "codex-cli"
        );
        assert_eq!(
            classify(&v(&["/opt/homebrew/bin/claude"]), false),
            "headless"
        );
        assert_eq!(classify(&[], false), "headless");
    }

    #[test]
    fn a_kernel_driven_run_is_headless_even_under_the_desktop_app() {
        assert_eq!(classify(&v(DESKTOP), true), "headless");
    }
}
