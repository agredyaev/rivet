//! OS-enforced isolation for commands launched by native tunnel sessions.
//!
//! Rivet itself keeps the OpenAI tunnel connection outside this sandbox.
//! Sandboxed children can modify the current workspace, their private
//! per-workspace cache, and explicit startup-granted command paths. Other host
//! toolchain paths are read-only. Network is opt-in.

use crate::profile;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

fn is_git(executable: &Path) -> bool {
    executable.file_name().and_then(|name| name.to_str()) == Some("git")
}

fn git_metadata_paths(workspace: &Path) -> Vec<PathBuf> {
    let dot_git = workspace.join(".git");
    let Ok(pointer) = fs::read_to_string(&dot_git) else {
        return Vec::new();
    };
    let Some(value) = pointer.trim().strip_prefix("gitdir:") else {
        return Vec::new();
    };

    let raw_gitdir = PathBuf::from(value.trim());
    let gitdir = if raw_gitdir.is_absolute() {
        raw_gitdir
    } else {
        workspace.join(raw_gitdir)
    };
    let gitdir = fs::canonicalize(&gitdir).unwrap_or(gitdir);

    let mut paths = vec![gitdir.clone()];
    if let Ok(common) = fs::read_to_string(gitdir.join("commondir")) {
        let raw_common = PathBuf::from(common.trim());
        let common = if raw_common.is_absolute() {
            raw_common
        } else {
            gitdir.join(raw_common)
        };
        let common = fs::canonicalize(&common).unwrap_or(common);
        if common != gitdir {
            paths.push(common);
        }
    }
    paths
}

fn cache_root(workspace: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    let base = home_dir()
        .map(|home| home.join("Library/Caches/rivet"))
        .unwrap_or_else(|| PathBuf::from("/private/tmp/rivet"));
    #[cfg(target_os = "linux")]
    let base = home_dir()
        .map(|home| home.join(".cache/rivet"))
        .unwrap_or_else(|| PathBuf::from("/tmp/rivet"));

    let mut hash = Sha256::new();
    hash.update(workspace.as_os_str().as_bytes());
    let digest = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    base.join(&digest[..16])
}

fn ensure_cache(workspace: &Path) -> Result<PathBuf, String> {
    let root = cache_root(workspace);
    for name in ["home", "tmp", "cargo-home"] {
        fs::create_dir_all(root.join(name)).map_err(|error| {
            format!("failed to create sandbox cache {}: {error}", root.display())
        })?;
    }

    if let Some(host_cargo) = home_dir().map(|home| home.join(".cargo")) {
        let cargo_home = root.join("cargo-home");
        for name in ["registry"] {
            let source = host_cargo.join(name);
            if !source.is_dir() {
                continue;
            }

            let target = cargo_home.join(name);
            match fs::symlink_metadata(&target) {
                Ok(metadata) => {
                    let points_to_source = metadata.file_type().is_symlink()
                        && fs::read_link(&target)
                            .map(|path| path == source)
                            .unwrap_or(false);
                    if !points_to_source {
                        return Err(format!(
                            "sandbox Cargo cache path {} already exists with an unexpected target",
                            target.display()
                        ));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    symlink(&source, &target).map_err(|error| {
                        format!(
                            "failed to expose read-only Cargo cache {}: {error}",
                            source.display()
                        )
                    })?;
                }
                Err(error) => {
                    return Err(format!(
                        "failed to inspect sandbox Cargo cache {}: {error}",
                        target.display()
                    ));
                }
            }
        }
    }

    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("failed to secure sandbox cache {}: {error}", root.display()))?;
    Ok(root)
}

pub fn doctor() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if Path::new("/usr/bin/sandbox-exec").is_file() {
            Ok(())
        } else {
            Err("macOS sandbox-exec is unavailable".into())
        }
    }

    #[cfg(target_os = "linux")]
    {
        let bwrap = bubblewrap().ok_or_else(|| {
            "bubblewrap is required for native Rivet sessions on Linux/WSL2 (install package: bubblewrap)"
                .to_owned()
        })?;
        let output = Command::new(bwrap)
            .args(["--unshare-user", "--ro-bind", "/", "/", "/usr/bin/true"])
            .output()
            .map_err(|error| format!("failed to start bubblewrap: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Permission denied") {
            return Err(
                "bubblewrap cannot create a user namespace. On Ubuntu 24.04, install apparmor-profiles and load /usr/share/apparmor/extra-profiles/bwrap-userns-restrict"
                    .into(),
            );
        }
        Err(format!(
            "bubblewrap sandbox preflight failed: {}",
            stderr.trim()
        ))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err("native Rivet sessions are supported on macOS and Linux/WSL2 only".into())
    }
}

pub fn apply_environment(command: &mut Command, workspace: &Path) {
    let cache = cache_root(workspace);
    command.env("HOME", cache.join("home"));
    command.env("TMPDIR", cache.join("tmp"));
    command.env("CARGO_HOME", cache.join("cargo-home"));

    if let Some(rustup) = home_dir().map(|home| home.join(".rustup"))
        && rustup.is_dir()
    {
        command.env("RUSTUP_HOME", rustup);
    }

    #[cfg(target_os = "macos")]
    if let Some(developer_dir) = macos_developer_dir() {
        command.env("DEVELOPER_DIR", &developer_dir);
        for sdk in [
            developer_dir.join("SDKs/MacOSX.sdk"),
            developer_dir.join("Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk"),
        ] {
            if sdk.is_dir() {
                command.env("SDKROOT", sdk);
                break;
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_developer_dir() -> Option<PathBuf> {
    let selector = Path::new("/var/select/developer_dir");
    if let Ok(target) = fs::read_link(selector) {
        let resolved = if target.is_absolute() {
            target
        } else {
            selector.parent()?.join(target)
        };
        if resolved.is_dir() {
            return Some(resolved);
        }
    }

    let command_line_tools = PathBuf::from("/Library/Developer/CommandLineTools");
    command_line_tools.is_dir().then_some(command_line_tools)
}

#[cfg(test)]
pub fn command(
    executable: &Path,
    args: &[String],
    workspace: &Path,
    cwd: &Path,
    network: bool,
) -> Result<Command, String> {
    command_with_paths(executable, args, workspace, cwd, &[], network)
}

pub fn command_with_paths(
    executable: &Path,
    args: &[String],
    workspace: &Path,
    cwd: &Path,
    command_paths: &[PathBuf],
    network: bool,
) -> Result<Command, String> {
    let cache_started = Instant::now();
    ensure_cache(workspace)?;
    let cache_us = cache_started.elapsed().as_micros();
    profile::event(|| {
        serde_json::json!({
            "event": "sandbox_cache",
            "cache_us": cache_us,
            "self_peak_rss_bytes": profile::self_peak_rss_bytes()
        })
    });

    #[cfg(target_os = "macos")]
    {
        macos_command(executable, args, workspace, cwd, command_paths, network)
    }

    #[cfg(target_os = "linux")]
    {
        linux_command(executable, args, workspace, cwd, command_paths, network)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (executable, args, workspace, cwd, command_paths, network);
        Err("native sandbox is supported on macOS and Linux/WSL2 only".into())
    }
}

fn existing(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    paths.into_iter().filter(|path| path.exists()).collect()
}

fn toolchain_paths() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    existing([
        home.join(".cargo/bin"),
        home.join(".cargo/registry"),
        home.join(".rustup"),
        home.join(".local/bin"),
        home.join(".local/share/uv"),
    ])
}

#[cfg(target_os = "macos")]
fn seatbelt_literal(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
fn allow_read_rule(path: &Path) -> String {
    let mut rule = String::new();
    for ancestor in path
        .ancestors()
        .skip(1)
        .filter(|path| *path != Path::new("/"))
    {
        rule.push_str(&format!(
            "(allow file-read-metadata (literal \"{}\"))\n",
            seatbelt_literal(ancestor)
        ));
    }
    let filter = if path.is_dir() { "subpath" } else { "literal" };
    rule.push_str(&format!(
        "(allow file-read* ({filter} \"{}\"))\n",
        seatbelt_literal(path)
    ));
    rule
}

#[cfg(target_os = "macos")]
fn macos_profile(
    executable: &Path,
    workspace: &Path,
    command_paths: &[PathBuf],
    network: bool,
) -> String {
    let workspace = fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let mut profile = String::from(
        r#"(version 1)
(deny default)

(allow process-exec)
(allow process-fork)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow sysctl-read)
(allow sysctl-write (sysctl-name "kern.grade_cputype"))
(allow iokit-open (iokit-registry-entry-class "RootDomainUserClient"))
(allow ipc-posix-sem)
(allow ipc-sysv-sem)
(allow ipc-posix-shm-read-data ipc-posix-shm-write-create ipc-posix-shm-write-unlink)
(allow pseudo-tty)

(allow mach-lookup
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.system.opendirectoryd.membership")
  (global-name "com.apple.bsd.dirhelper"))

(allow file-read* (literal "/"))
(allow file-read*
  (subpath "/bin")
  (subpath "/sbin")
  (subpath "/usr")
  (subpath "/System")
  (subpath "/Library")
  (subpath "/etc")
  (subpath "/private/etc")
  (subpath "/private/var/db")
  (subpath "/opt/homebrew")
  (subpath "/usr/local"))
(allow file-read* file-write*
  (literal "/dev/null")
  (literal "/dev/zero")
  (literal "/dev/random")
  (literal "/dev/urandom")
  (literal "/dev/ptmx"))
(allow file-read* file-write* file-ioctl
  (regex #"^/dev/ttys[0-9]+"))
(deny file-ioctl (ioctl-command TIOCSTI))

"#,
    );

    if network {
        profile.push_str(
            "(allow network-outbound)\n\
(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))\n\
(allow mach-lookup\n\
  (global-name \"com.apple.SystemConfiguration.DNSConfiguration\")\n\
  (global-name \"com.apple.SystemConfiguration.configd\")\n\
  (global-name \"com.apple.networkd\")\n\
  (global-name \"com.apple.trustd.agent\"))\n",
        );
    }

    profile.push_str(&allow_read_rule(&workspace));
    profile.push_str(&format!(
        "(allow file-read* file-write* (subpath \"{}\"))\n",
        seatbelt_literal(&workspace)
    ));
    for path in command_paths {
        profile.push_str(&allow_read_rule(path));
        let filter = if path.is_dir() { "subpath" } else { "literal" };
        let literal = seatbelt_literal(path);
        profile.push_str(&format!(
            "(allow file-read* file-write* ({filter} \"{literal}\"))\n"
        ));
        if network {
            let socket_filter = if path.is_dir() {
                format!("(subpath \"{literal}\")")
            } else {
                format!("(path-literal \"{literal}\")")
            };
            profile.push_str(&format!(
                "(allow network-outbound (remote unix-socket {socket_filter}))\n"
            ));
        }
    }

    profile.push_str(&allow_read_rule(executable));
    if let Ok(canonical) = fs::canonicalize(executable)
        && canonical != executable
    {
        profile.push_str(&allow_read_rule(&canonical));
    }
    for path in toolchain_paths() {
        profile.push_str(&allow_read_rule(&path));
    }
    profile.push_str(&allow_read_rule(Path::new("/var/select")));
    profile.push_str(&allow_read_rule(Path::new("/private/var/select")));

    if is_git(executable) {
        for path in git_metadata_paths(&workspace) {
            profile.push_str(&allow_read_rule(&path));
            profile.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                seatbelt_literal(&path)
            ));
        }
        if let Some(xcode_cache) =
            home_dir().map(|home| home.join("Library/Caches/com.apple.dt.Xcode"))
            && xcode_cache.is_dir()
        {
            profile.push_str(&allow_read_rule(&xcode_cache));
            profile.push_str(&format!(
                "(allow file-write* (subpath \"{}\"))\n",
                seatbelt_literal(&xcode_cache)
            ));
        }
    }

    let cache = cache_root(&workspace);
    profile.push_str(&allow_read_rule(&cache));
    profile.push_str(&format!(
        "(allow file-write* (subpath \"{}\"))\n",
        seatbelt_literal(&cache)
    ));
    profile
}

#[cfg(target_os = "macos")]
fn macos_command(
    executable: &Path,
    args: &[String],
    workspace: &Path,
    cwd: &Path,
    command_paths: &[PathBuf],
    network: bool,
) -> Result<Command, String> {
    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
    if !Path::new(SANDBOX_EXEC).is_file() {
        return Err("macOS sandbox-exec is unavailable".into());
    }

    let profile_started = Instant::now();
    let seatbelt = macos_profile(executable, workspace, command_paths, network);
    let profile_us = profile_started.elapsed().as_micros();

    let command_started = Instant::now();
    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(seatbelt)
        .arg(executable)
        .args(args)
        .current_dir(cwd);
    let command_build_us = command_started.elapsed().as_micros();
    profile::event(|| {
        serde_json::json!({
            "event": "sandbox_prepare",
            "platform": "macos",
            "executable": executable.file_name().and_then(|name| name.to_str()),
            "profile_us": profile_us,
            "command_build_us": command_build_us,
            "self_peak_rss_bytes": profile::self_peak_rss_bytes()
        })
    });
    Ok(command)
}

#[cfg(target_os = "linux")]
fn bubblewrap() -> Option<&'static str> {
    ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
}

#[cfg(target_os = "linux")]
fn hidden_linux_roots() -> Vec<PathBuf> {
    let mut roots: Vec<_> = ["/home", "/root", "/mnt", "/media", "/run"]
        .into_iter()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    if let Some(home) = home_dir()
        && !roots.iter().any(|root| home.starts_with(root))
    {
        roots.push(home);
    }
    roots
}

#[cfg(target_os = "linux")]
fn linux_command(
    executable: &Path,
    args: &[String],
    workspace: &Path,
    cwd: &Path,
    command_paths: &[PathBuf],
    network: bool,
) -> Result<Command, String> {
    let bwrap = bubblewrap().ok_or(
        "bubblewrap is required for native Rivet sessions on Linux/WSL2 (install package: bubblewrap)",
    )?;

    let mut command = Command::new(bwrap);
    command.args([
        "--die-with-parent",
        "--new-session",
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--cap-drop",
        "ALL",
        "--ro-bind",
        "/",
        "/",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
    ]);
    if !network {
        command.arg("--unshare-net");
    }

    for path in hidden_linux_roots() {
        command.arg("--tmpfs").arg(path);
    }
    command.arg("--tmpfs").arg("/tmp");

    if network {
        for resolver in [
            "/run/systemd/resolve",
            "/run/resolvconf",
            "/run/NetworkManager/resolv.conf",
        ] {
            command.arg("--ro-bind-try").arg(resolver).arg(resolver);
        }
    }

    command.arg("--ro-bind-try").arg(executable).arg(executable);
    if let Ok(canonical) = fs::canonicalize(executable)
        && canonical != executable
    {
        command.arg("--ro-bind-try").arg(&canonical).arg(&canonical);
    }
    for path in toolchain_paths() {
        command.arg("--ro-bind-try").arg(&path).arg(&path);
    }
    for path in command_paths {
        command.arg("--bind").arg(path).arg(path);
    }
    if is_git(executable) {
        for path in git_metadata_paths(workspace) {
            command.arg("--bind").arg(&path).arg(&path);
        }
    }

    let cache = cache_root(workspace);
    command.arg("--bind").arg(&cache).arg(&cache);
    command
        .arg("--bind")
        .arg(workspace)
        .arg(workspace)
        .arg("--chdir")
        .arg(cwd)
        .arg(executable)
        .args(args);

    Ok(command)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    #[cfg(target_os = "macos")]
    use std::os::unix::net::UnixListener;
    use std::{
        net::TcpListener,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn test_workspace() -> (PathBuf, PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base =
            env::temp_dir().join(format!("rivet-sandbox-test-{}-{nonce}", std::process::id()));
        let workspace = base.join("workspace");
        let sibling = base.join("sibling");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        (base, workspace, sibling)
    }

    #[test]
    fn linked_worktree_git_metadata_is_resolved_narrowly() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = env::temp_dir().join(format!(
            "rivet-git-worktree-test-{}-{nonce}",
            std::process::id()
        ));
        let workspace = base.join("worktree");
        let common = base.join("repo.git");
        let worktree_git = common.join("worktrees/worktree");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&worktree_git).unwrap();
        fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();
        fs::write(worktree_git.join("commondir"), "../..\n").unwrap();

        let paths = git_metadata_paths(&workspace);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], worktree_git.canonicalize().unwrap());
        assert_eq!(paths[1], common.canonicalize().unwrap());

        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_git_binds_linked_worktree_metadata() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = env::temp_dir().join(format!(
            "rivet-linux-git-worktree-test-{}-{nonce}",
            std::process::id()
        ));
        let workspace = base.join("worktree");
        let common = base.join("repo.git");
        let worktree_git = common.join("worktrees/worktree");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&worktree_git).unwrap();
        fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();
        fs::write(worktree_git.join("commondir"), "../..\n").unwrap();

        let command = linux_command(
            Path::new("/usr/bin/git"),
            &["status".to_owned()],
            &workspace,
            &workspace,
            &[],
            false,
        )
        .unwrap();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        for path in [
            worktree_git.canonicalize().unwrap(),
            common.canonicalize().unwrap(),
        ] {
            let value = path.to_string_lossy().into_owned();
            assert!(
                args.windows(3)
                    .any(|window| window == ["--bind", value.as_str(), value.as_str()])
            );
        }

        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandboxed_git_runs_in_linked_worktree() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = env::temp_dir().join(format!(
            "rivet-linux-git-integration-test-{}-{nonce}",
            std::process::id()
        ));
        let repository = base.join("repository");
        let workspace = base.join("worktree");
        fs::create_dir_all(&repository).unwrap();

        let git = Path::new("/usr/bin/git");
        let run_git = |cwd: &Path, args: &[&str]| {
            let output = std::process::Command::new(git)
                .current_dir(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };

        run_git(&repository, &["init"]);
        fs::write(repository.join("tracked.txt"), "tracked\n").unwrap();
        run_git(&repository, &["add", "tracked.txt"]);
        run_git(
            &repository,
            &[
                "-c",
                "user.name=Rivet Test",
                "-c",
                "user.email=rivet@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
        );
        run_git(
            &repository,
            &[
                "worktree",
                "add",
                "-b",
                "rivet-test",
                workspace.to_str().unwrap(),
            ],
        );

        let mut sandboxed = command(
            git,
            &["status".to_owned(), "--short".to_owned()],
            &workspace,
            &workspace,
            false,
        )
        .unwrap();
        apply_environment(&mut sandboxed, &workspace);
        let output = sandboxed.output().unwrap();
        assert!(
            output.status.success(),
            "sandboxed git status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_allows_workspace_and_denies_sibling() {
        let (base, workspace, sibling) = test_workspace();
        fs::write(workspace.join("allowed"), "ok").unwrap();
        fs::write(sibling.join("denied"), "no").unwrap();

        doctor().unwrap();
        let mut allowed = command(
            Path::new("/bin/cat"),
            &[workspace.join("allowed").display().to_string()],
            &workspace,
            &workspace,
            false,
        )
        .unwrap();
        apply_environment(&mut allowed, &workspace);
        let allowed = allowed.output().unwrap();
        assert!(
            allowed.status.success(),
            "{}",
            String::from_utf8_lossy(&allowed.stderr)
        );
        assert_eq!(allowed.stdout, b"ok");

        let mut denied = command(
            Path::new("/bin/cat"),
            &[sibling.join("denied").display().to_string()],
            &workspace,
            &workspace,
            false,
        )
        .unwrap();
        apply_environment(&mut denied, &workspace);
        assert!(!denied.output().unwrap().status.success());

        let created = workspace.join("created");
        let mut write = command(
            Path::new("/usr/bin/touch"),
            &[created.display().to_string()],
            &workspace,
            &workspace,
            false,
        )
        .unwrap();
        apply_environment(&mut write, &workspace);
        assert!(write.output().unwrap().status.success());
        assert!(created.exists());

        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_profile_allows_only_explicit_command_path() {
        let (base, workspace, sibling) = test_workspace();
        let allowed_dir = sibling.join("allowed");
        let denied_dir = sibling.join("denied");
        fs::create_dir_all(&allowed_dir).unwrap();
        fs::create_dir_all(&denied_dir).unwrap();

        let profile = macos_profile(
            Path::new("/bin/cat"),
            &workspace,
            std::slice::from_ref(&allowed_dir),
            false,
        );
        let allowed = seatbelt_literal(&allowed_dir);
        let denied = seatbelt_literal(&denied_dir);
        assert!(profile.contains(&format!(
            "(allow file-read* file-write* (subpath \"{allowed}\"))"
        )));
        assert!(!profile.contains(&denied));

        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_network_allows_unix_sockets_only_under_explicit_command_paths() {
        let (base, workspace, sibling) = test_workspace();
        let allowed_dir = sibling.join("allowed");
        let denied_dir = sibling.join("denied");
        fs::create_dir_all(&allowed_dir).unwrap();
        fs::create_dir_all(&denied_dir).unwrap();

        let network_profile = macos_profile(
            Path::new("/bin/cat"),
            &workspace,
            std::slice::from_ref(&allowed_dir),
            true,
        );
        let allowed = seatbelt_literal(&allowed_dir);
        let denied = seatbelt_literal(&denied_dir);
        let socket_grant =
            format!("(allow network-outbound (remote unix-socket (subpath \"{allowed}\")))");
        assert!(network_profile.contains(&socket_grant));
        assert!(!network_profile.contains(&denied));

        let offline_profile = macos_profile(
            Path::new("/bin/cat"),
            &workspace,
            std::slice::from_ref(&allowed_dir),
            false,
        );
        assert!(!offline_profile.contains(&socket_grant));

        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_profile_binds_only_explicit_command_path() {
        let (base, workspace, sibling) = test_workspace();
        let allowed_dir = sibling.join("allowed");
        let denied_dir = sibling.join("denied");
        fs::create_dir_all(&allowed_dir).unwrap();
        fs::create_dir_all(&denied_dir).unwrap();

        let command = linux_command(
            Path::new("/bin/cat"),
            &[],
            &workspace,
            &workspace,
            std::slice::from_ref(&allowed_dir),
            false,
        )
        .unwrap();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let allowed = allowed_dir.to_string_lossy().into_owned();
        let denied = denied_dir.to_string_lossy().into_owned();
        assert!(
            args.windows(3)
                .any(|window| { window == ["--bind", allowed.as_str(), allowed.as_str()] })
        );
        assert!(!args.iter().any(|arg| arg == &denied));

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn sandbox_denies_host_home() {
        let workspace = env::current_dir().unwrap().canonicalize().unwrap();
        let Some(home) = home_dir() else {
            return;
        };
        if home == workspace || home.starts_with(&workspace) {
            return;
        }
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let secret = home.join(format!(
            ".rivet-sandbox-secret-{}-{nonce}",
            std::process::id()
        ));
        fs::write(&secret, "secret").unwrap();

        let mut child = command(
            Path::new("/bin/cat"),
            &[secret.display().to_string()],
            &workspace,
            &workspace,
            false,
        )
        .unwrap();
        apply_environment(&mut child, &workspace);
        let output = child.output().unwrap();
        let _ = fs::remove_file(&secret);
        assert!(!output.status.success());
    }

    #[test]
    fn sandbox_cache_is_workspace_scoped() {
        let (base_a, workspace_a, _) = test_workspace();
        let (base_b, workspace_b, _) = test_workspace();
        assert_ne!(cache_root(&workspace_a), cache_root(&workspace_b));
        fs::remove_dir_all(base_a).unwrap();
        fs::remove_dir_all(base_b).unwrap();
    }

    #[test]
    fn sandbox_runs_uv_python_when_available() {
        let workspace = env::current_dir().unwrap().canonicalize().unwrap();
        doctor().unwrap();
        let Some(path) = env::var_os("PATH") else {
            return;
        };
        let Some(uv) = env::split_paths(&path)
            .map(|dir| dir.join("uv"))
            .find(|path| path.is_file())
        else {
            return;
        };

        let args = [
            "run".to_owned(),
            "python".to_owned(),
            "-c".to_owned(),
            "print('sandbox-ok')".to_owned(),
        ];
        let mut command = command(&uv, &args, &workspace, &workspace, false).unwrap();
        apply_environment(&mut command, &workspace);
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "sandbox-ok");
    }

    #[test]
    fn sandbox_runs_cargo_toolchain() {
        let workspace = env::current_dir().unwrap().canonicalize().unwrap();
        doctor().unwrap();
        let Some(cargo) = env::var_os("CARGO").map(PathBuf::from) else {
            return;
        };
        let mut cargo_version = command(
            &cargo,
            &["--version".to_owned()],
            &workspace,
            &workspace,
            false,
        )
        .unwrap();
        apply_environment(&mut cargo_version, &workspace);
        let output = cargo_version.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let project = workspace.join(format!(
            ".rivet-sandbox-cargo-check-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&project).unwrap();
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"rivet_sandbox_check\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[lib]\npath = \"lib.rs\"\n",
        )
        .unwrap();
        fs::write(project.join("lib.rs"), "pub fn check() -> bool { true }\n").unwrap();

        let args = [
            "check".to_owned(),
            "--offline".to_owned(),
            "--manifest-path".to_owned(),
            project.join("Cargo.toml").display().to_string(),
        ];
        let mut check = command(&cargo, &args, &workspace, &workspace, false).unwrap();
        apply_environment(&mut check, &workspace);
        let output = check.output().unwrap();
        let _ = fs::remove_dir_all(&project);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_network_reaches_unix_socket_in_explicit_command_path() {
        let workspace = env::current_dir().unwrap().canonicalize().unwrap();
        doctor().unwrap();
        let python = env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("python3"))
            .find(|path| path.is_file())
            .expect("python3 is required by the test suite");

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket_dir = home_dir()
            .expect("HOME is required by the macOS sandbox test")
            .join(format!(
                ".rivet-unix-socket-test-{}-{nonce}",
                std::process::id()
            ));
        fs::create_dir_all(&socket_dir).unwrap();
        let socket_path = socket_dir.join("docker.sock");
        let _listener = UnixListener::bind(&socket_path).unwrap();
        let socket = socket_path.display().to_string();

        let denied_script = format!(
            "import socket,sys; s=socket.socket(socket.AF_UNIX); sys.exit(0 if s.connect_ex({socket:?}) != 0 else 1)"
        );
        let mut denied = command_with_paths(
            &python,
            &["-c".to_owned(), denied_script],
            &workspace,
            &workspace,
            std::slice::from_ref(&socket_dir),
            false,
        )
        .unwrap();
        apply_environment(&mut denied, &workspace);
        let denied = denied.output().unwrap();
        assert!(
            denied.status.success(),
            "{}",
            String::from_utf8_lossy(&denied.stderr)
        );

        let allowed_script = format!(
            "import socket,sys; s=socket.socket(socket.AF_UNIX); sys.exit(0 if s.connect_ex({socket:?}) == 0 else 1)"
        );
        let mut allowed = command_with_paths(
            &python,
            &["-c".to_owned(), allowed_script],
            &workspace,
            &workspace,
            std::slice::from_ref(&socket_dir),
            true,
        )
        .unwrap();
        apply_environment(&mut allowed, &workspace);
        let allowed = allowed.output().unwrap();
        let _ = fs::remove_file(&socket_path);
        let _ = fs::remove_dir(&socket_dir);
        assert!(
            allowed.status.success(),
            "{}",
            String::from_utf8_lossy(&allowed.stderr)
        );
    }

    #[test]
    fn sandbox_network_is_opt_in() {
        let workspace = env::current_dir().unwrap().canonicalize().unwrap();
        doctor().unwrap();
        let python = env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("python3"))
            .find(|path| path.is_file())
            .expect("python3 is required by the test suite");

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();

        let denied_script = format!(
            "import socket,sys; s=socket.socket(); sys.exit(0 if s.connect_ex(('127.0.0.1',{port})) != 0 else 1)"
        );
        let mut denied = command(
            &python,
            &["-c".to_owned(), denied_script],
            &workspace,
            &workspace,
            false,
        )
        .unwrap();
        apply_environment(&mut denied, &workspace);
        let denied = denied.output().unwrap();
        assert!(
            denied.status.success(),
            "{}",
            String::from_utf8_lossy(&denied.stderr)
        );

        let allowed_script = format!(
            "import socket,sys; s=socket.socket(); sys.exit(0 if s.connect_ex(('127.0.0.1',{port})) == 0 else 1)"
        );
        let mut allowed = command(
            &python,
            &["-c".to_owned(), allowed_script],
            &workspace,
            &workspace,
            true,
        )
        .unwrap();
        apply_environment(&mut allowed, &workspace);
        let allowed = allowed.output().unwrap();
        assert!(
            allowed.status.success(),
            "{}",
            String::from_utf8_lossy(&allowed.stderr)
        );
    }
}
