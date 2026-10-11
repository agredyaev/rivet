//! OS-enforced isolation for commands launched by native tunnel sessions.
//!
//! Rivet itself keeps the OpenAI tunnel connection outside this sandbox.
//! Sandboxed children can modify only the current workspace and their private
//! per-workspace cache. Host toolchains are read-only. Network is opt-in.

use sha2::{Digest, Sha256};
use std::{
    env, fs,
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
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
    let digest = format!("{:x}", hash.finalize());
    base.join(&digest[..16])
}

fn ensure_cache(workspace: &Path) -> Result<PathBuf, String> {
    let root = cache_root(workspace);
    for name in ["home", "tmp"] {
        fs::create_dir_all(root.join(name)).map_err(|error| {
            format!("failed to create sandbox cache {}: {error}", root.display())
        })?;
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

    if let Some(rustup) = home_dir().map(|home| home.join(".rustup"))
        && rustup.is_dir()
    {
        command.env("RUSTUP_HOME", rustup);
    }
}

pub fn command(
    executable: &Path,
    args: &[String],
    workspace: &Path,
    cwd: &Path,
    network: bool,
) -> Result<Command, String> {
    ensure_cache(workspace)?;

    #[cfg(target_os = "macos")]
    {
        macos_command(executable, args, workspace, cwd, network)
    }

    #[cfg(target_os = "linux")]
    {
        linux_command(executable, args, workspace, cwd, network)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (executable, args, workspace, cwd, network);
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
fn macos_profile(executable: &Path, workspace: &Path, network: bool) -> String {
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

    profile.push_str(&allow_read_rule(executable));
    if let Ok(canonical) = fs::canonicalize(executable)
        && canonical != executable
    {
        profile.push_str(&allow_read_rule(&canonical));
    }
    for path in toolchain_paths() {
        profile.push_str(&allow_read_rule(&path));
    }
    for selector in [
        Path::new("/var/select/developer_dir"),
        Path::new("/private/var/select/developer_dir"),
    ] {
        profile.push_str(&allow_read_rule(selector));
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
    network: bool,
) -> Result<Command, String> {
    const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
    if !Path::new(SANDBOX_EXEC).is_file() {
        return Err("macOS sandbox-exec is unavailable".into());
    }

    let mut command = Command::new(SANDBOX_EXEC);
    command
        .arg("-p")
        .arg(macos_profile(executable, workspace, network))
        .arg(executable)
        .args(args)
        .current_dir(cwd);
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
    use std::{
        net::TcpListener,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn test_workspace() -> (PathBuf, PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = env::current_dir().unwrap().join(format!(
            ".rivet-sandbox-test-{}-{nonce}",
            std::process::id()
        ));
        let workspace = base.join("workspace");
        let sibling = base.join("sibling");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        (base, workspace, sibling)
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
        let mut version_command = command(
            &cargo,
            &["--version".to_owned()],
            &workspace,
            &workspace,
            false,
        )
        .unwrap();
        apply_environment(&mut version_command, &workspace);
        let output = version_command.output().unwrap();
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
