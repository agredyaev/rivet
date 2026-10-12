//! Embedded defaults and startup scope validation.
//!
//! Executables are resolved once, and allowed roots are kept open as directory
//! capabilities so filesystem operations can remain relative to those roots.
use cap_std::{ambient_authority, fs::Dir};
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

pub const PROCESS_RETENTION_SLOTS: usize = 32;
pub const MAX_RUNNING_PROCESSES: usize = 10;

pub struct Limits {
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_file_read_bytes: usize,
    pub max_directory_entries: usize,
    pub max_running_processes: usize,
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_stdout_bytes: 2 * 1024 * 1024,
            max_stderr_bytes: 2 * 1024 * 1024,
            max_file_read_bytes: 4 * 1024 * 1024,
            max_directory_entries: 1000,
            max_running_processes: 8,
            default_timeout_ms: 120_000,
            max_timeout_ms: 1_800_000,
        }
    }
}

pub struct Environment {
    pub pass: Vec<String>,
    pub allow_override: Vec<String>,
}

pub struct CommandConfig {
    pub executable: String,
    pub allowed_subcommands: Option<Vec<String>>,
}

pub struct Root {
    pub path: PathBuf,
    configured: PathBuf,
    pub dir: Dir,
}
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CommandId(u16);

impl CommandId {
    pub const ZERO: Self = Self(0);
}

pub struct CommandEntry {
    pub name: String,
    pub path: PathBuf,
    pub allowed_subcommands: Option<Box<[String]>>,
}

pub struct Config {
    pub roots: Vec<Root>,
    pub limits: Limits,
    pub environment: Environment,
    pub commands: Box<[CommandEntry]>,
    pub sandbox_commands: bool,
    pub sandbox_network: bool,
}

fn env_name(s: &str) -> bool {
    !s.is_empty() && !s.contains(['=', '\0'])
}

fn executable(input: &str) -> Result<PathBuf, String> {
    if input.is_empty() || input.contains('\0') {
        return Err("invalid executable".into());
    }
    let candidates: Vec<PathBuf> = if input.contains(['/', '\\']) {
        vec![PathBuf::from(input)]
    } else {
        env::split_paths(&env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join(input))
            .collect()
    };
    for path in candidates {
        let path = if path.is_absolute() {
            path
        } else if let Ok(current_dir) = env::current_dir() {
            current_dir.join(path)
        } else {
            continue;
        };
        if let Ok(canonical) = fs::canonicalize(&path)
            && let Ok(meta) = fs::metadata(&canonical)
            && executable_file(&meta)
        {
            // Keep the resolved path's final component: tools such as
            // rustup select their proxy behavior from argv[0] (`cargo`).
            return Ok(path);
        }
    }
    Err(format!(
        "executable {input:?} was not found or is not executable"
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::executable;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn executable_preserves_symlink_name_when_launched() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("rivet-executable-{}-{nonce}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let target = dir.join("rustup");
        let shim = dir.join("cargo");
        fs::write(&target, "#!/bin/sh\nprintf '%s\\n' \"$0\" \"$@\"\n").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&target, &shim).unwrap();

        let resolved = executable(shim.to_str().unwrap()).unwrap();
        let output = Command::new(&resolved)
            .args(["check", "-p", "mesh-compiler"])
            .output()
            .unwrap();

        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let mut lines = stdout.lines();
        assert_eq!(lines.next(), Some(shim.to_str().unwrap()));
        assert_eq!(lines.collect::<Vec<_>>(), ["check", "-p", "mesh-compiler"]);
        fs::remove_dir_all(dir).unwrap();
    }
}

fn executable_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

impl Config {
    pub fn new(
        cli_roots: Vec<PathBuf>,
        command_specs: BTreeMap<String, CommandConfig>,
    ) -> Result<Self, String> {
        Self::build(
            Limits::default(),
            Environment {
                pass: vec!["PATH".into(), "HOME".into(), "TMPDIR".into(), "LANG".into()],
                allow_override: vec!["RUST_LOG".into()],
            },
            cli_roots,
            command_specs,
            false,
            false,
        )
    }

    pub fn native_session(
        cli_roots: Vec<PathBuf>,
        command_specs: BTreeMap<String, CommandConfig>,
        sandbox_network: bool,
    ) -> Result<Self, String> {
        Self::build(
            Limits::default(),
            Environment {
                pass: vec!["PATH".into(), "LANG".into()],
                allow_override: vec!["RUST_LOG".into()],
            },
            cli_roots,
            command_specs,
            true,
            sandbox_network,
        )
    }

    fn build(
        limits: Limits,
        environment: Environment,
        cli_roots: Vec<PathBuf>,
        command_specs: BTreeMap<String, CommandConfig>,
        sandbox_commands: bool,
        sandbox_network: bool,
    ) -> Result<Self, String> {
        if limits.max_stdout_bytes == 0
            || limits.max_stderr_bytes == 0
            || limits.max_file_read_bytes == 0
            || limits.max_file_read_bytes > 64 * 1024 * 1024
            || limits.max_directory_entries == 0
            || limits.max_running_processes == 0
            || limits.max_running_processes > MAX_RUNNING_PROCESSES
            || limits.default_timeout_ms == 0
            || limits.max_timeout_ms == 0
            || limits.max_timeout_ms > u32::MAX as u64
            || limits.default_timeout_ms > limits.max_timeout_ms
        {
            return Err("limits must be positive, max_file_read_bytes <= 64 MiB, max_running_processes <= 10, and default_timeout_ms must not exceed max_timeout_ms".into());
        }
        if cli_roots.is_empty() {
            return Err("at least one --root is required".into());
        }
        let mut roots = Vec::new();
        for path in cli_roots {
            let path = if !path.is_absolute() {
                env::current_dir()
                    .map_err(|e| format!("failed to resolve --root: {e}"))?
                    .join(path)
            } else {
                path
            };
            if !path.is_absolute() {
                return Err(format!(
                    "--root must resolve to an absolute path: {}",
                    path.display()
                ));
            }
            let canonical =
                fs::canonicalize(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if roots.iter().any(|root: &Root| root.path == canonical) {
                return Err(format!("duplicate root: {}", canonical.display()));
            }
            let dir = Dir::open_ambient_dir(&canonical, ambient_authority())
                .map_err(|e| format!("{}: {e}", canonical.display()))?;
            roots.push(Root {
                path: canonical,
                configured: path,
                dir,
            });
        }
        // Prefer the most specific root when roots overlap.
        roots.sort_by(|a, b| {
            b.path
                .components()
                .count()
                .cmp(&a.path.components().count())
        });
        for name in environment.pass.iter().chain(&environment.allow_override) {
            if !env_name(name) {
                return Err(format!("invalid environment name: {name:?}"));
            }
        }
        if command_specs.len() > u16::MAX as usize + 1 {
            return Err("too many registered commands".into());
        }
        let mut commands = Vec::with_capacity(command_specs.len());
        for (name, command) in command_specs {
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(format!("invalid command name: {name:?}"));
            }
            let allowed_subcommands = match command.allowed_subcommands {
                Some(values)
                    if values.is_empty() || values.iter().any(|value| value.is_empty()) =>
                {
                    return Err(format!("{name}: allowed_subcommands must be nonempty"));
                }
                Some(values) => Some(values.into_boxed_slice()),
                None => None,
            };
            commands.push(CommandEntry {
                name,
                path: executable(&command.executable)?,
                allowed_subcommands,
            });
        }
        Ok(Self {
            roots,
            limits,
            environment,
            commands: commands.into_boxed_slice(),
            sandbox_commands,
            sandbox_network,
        })
    }

    pub fn command(&self, name: &str) -> Option<(CommandId, &CommandEntry)> {
        let index = self
            .commands
            .binary_search_by(|entry| entry.name.as_str().cmp(name))
            .ok()?;
        Some((CommandId(index as u16), &self.commands[index]))
    }

    pub fn command_name(&self, id: CommandId) -> &str {
        &self.commands[id.0 as usize].name
    }

    pub fn root_for<'a>(&'a self, path: &'a Path) -> Option<(&'a Root, &'a Path)> {
        if !path.is_absolute() {
            return None;
        }
        self.roots.iter().find_map(|root| {
            rivet::path_policy::relative_under(&root.path, path)
                .or_else(|| rivet::path_policy::relative_under(&root.configured, path))
                .map(|relative| (root, relative))
        })
    }

    pub fn doctor(&self) -> Result<(), String> {
        for root in &self.roots {
            root.dir
                .read_dir(".")
                .map_err(|e| format!("{}: {e}", root.path.display()))?;
            let path = std::ffi::CString::new(".").expect("static string");
            use std::os::fd::AsRawFd;
            // SAFETY: the directory fd is open and path is a valid NUL-terminated string.
            if unsafe {
                libc::faccessat(
                    root.dir.as_raw_fd(),
                    path.as_ptr(),
                    libc::R_OK | libc::W_OK | libc::X_OK,
                    0,
                )
            } < 0
            {
                return Err(format!(
                    "{}: root needs read, write, and search access",
                    root.path.display()
                ));
            }
        }
        for fd in [0, 1, 2] {
            // SAFETY: fcntl only inspects the fixed stdio descriptor number.
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
                return Err(format!("stdio fd {fd} is unavailable"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod native_session_tests {
    use super::{CommandConfig, Config};
    use std::{
        collections::BTreeMap,
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn native_session_uses_embedded_defaults_without_toml() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rivet-native-session-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();

        let config = Config::native_session(vec![root.clone()], BTreeMap::new(), false).unwrap();
        assert_eq!(config.limits.max_stdout_bytes, 2 * 1024 * 1024);
        assert_eq!(config.limits.max_stderr_bytes, 2 * 1024 * 1024);
        assert_eq!(config.limits.max_file_read_bytes, 4 * 1024 * 1024);
        assert_eq!(config.limits.max_directory_entries, 1000);
        assert_eq!(config.limits.max_running_processes, 8);
        assert_eq!(config.limits.default_timeout_ms, 120_000);
        assert_eq!(config.limits.max_timeout_ms, 1_800_000);
        assert_eq!(config.environment.pass, ["PATH", "LANG"]);
        assert_eq!(config.environment.allow_override, ["RUST_LOG"]);
        assert!(config.sandbox_commands);
        assert!(!config.sandbox_network);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn frozen_commands_use_sorted_dense_ids() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("rivet-command-ids-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();

        let command = |executable: &str| CommandConfig {
            executable: executable.into(),
            allowed_subcommands: None,
        };
        let mut commands = BTreeMap::new();
        commands.insert("zeta".into(), command("/bin/echo"));
        commands.insert("alpha".into(), command("/bin/echo"));
        commands.insert(
            "restricted".into(),
            CommandConfig {
                executable: "/bin/echo".into(),
                allowed_subcommands: Some(vec!["check".into(), "test".into()]),
            },
        );

        let config = Config::native_session(vec![root.clone()], commands, false).unwrap();
        assert_eq!(
            config
                .commands
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "restricted", "zeta"]
        );

        let (alpha, alpha_entry) = config.command("alpha").unwrap();
        let (_, restricted) = config.command("restricted").unwrap();
        let (zeta, _) = config.command("zeta").unwrap();
        assert!(alpha_entry.allowed_subcommands.is_none());
        assert_eq!(
            restricted.allowed_subcommands.as_deref().unwrap(),
            ["check", "test"]
        );
        assert_eq!(config.command_name(alpha), "alpha");
        assert_eq!(config.command_name(zeta), "zeta");
        assert_ne!(alpha.0, zeta.0);

        fs::remove_dir_all(root).unwrap();
    }
}
