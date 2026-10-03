//! TOML loading and startup validation.
//!
//! Executables are resolved once, and allowed roots are kept open as directory
//! capabilities so filesystem operations can remain relative to those roots.
use cap_std::{ambient_authority, fs::Dir};
use serde::Deserialize;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    pub limits: Limits,
    pub environment: Environment,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub max_file_read_bytes: usize,
    pub max_directory_entries: usize,
    pub max_running_processes: usize,
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub pass: Vec<String>,
    pub allow_override: Vec<String>,
}

pub struct CommandConfig {
    pub executable: String,
    pub allow_any_args: bool,
    pub allowed_subcommands: Option<Vec<String>>,
}

pub struct Root {
    pub path: PathBuf,
    configured: PathBuf,
    pub dir: Dir,
}
pub struct CommandEntry {
    pub path: PathBuf,
    pub allow_any_args: bool,
    pub allowed_subcommands: Vec<String>,
}
pub struct Config {
    pub roots: Vec<Root>,
    pub limits: Limits,
    pub environment: Environment,
    pub commands: BTreeMap<String, CommandEntry>,
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
        #[cfg(windows)]
        let paths: Vec<_> = if path.extension().is_some() {
            vec![path]
        } else {
            env::var("PATHEXT")
                .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
                .split(';')
                .map(|extension| PathBuf::from(format!("{}{extension}", path.display())))
                .collect()
        };
        #[cfg(not(windows))]
        let paths = vec![path];
        for path in paths {
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
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

impl Config {
    pub fn load(
        path: &Path,
        cli_roots: Vec<PathBuf>,
        command_specs: BTreeMap<String, CommandConfig>,
    ) -> Result<Self, String> {
        let source = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let file: ConfigFile = toml::from_str(&source).map_err(|e| e.to_string())?;
        let limits = file.limits;
        if limits.max_stdout_bytes == 0
            || limits.max_stderr_bytes == 0
            || limits.max_file_read_bytes == 0
            || limits.max_file_read_bytes > 64 * 1024 * 1024
            || limits.max_directory_entries == 0
            || limits.max_running_processes == 0
            || limits.default_timeout_ms == 0
            || limits.max_timeout_ms == 0
            || limits.max_timeout_ms > u32::MAX as u64
            || limits.default_timeout_ms > limits.max_timeout_ms
        {
            return Err("limits must be positive, max_file_read_bytes <= 64 MiB, and default_timeout_ms <= max_timeout_ms".into());
        }
        let configured_roots = cli_roots;
        if configured_roots.is_empty() {
            return Err("at least one --root is required".into());
        }
        let mut roots = Vec::new();
        let mut seen = BTreeSet::new();
        for path in configured_roots {
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
            if !seen.insert(canonical.clone()) {
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
        for name in file
            .environment
            .pass
            .iter()
            .chain(&file.environment.allow_override)
        {
            if !env_name(name) {
                return Err(format!("invalid environment name: {name:?}"));
            }
        }
        let mut commands = BTreeMap::new();
        for (name, command) in command_specs {
            if name.is_empty() || name.contains(char::is_whitespace) {
                return Err(format!("invalid command name: {name:?}"));
            }
            if command.allow_any_args == command.allowed_subcommands.is_some() {
                return Err(format!(
                    "{name}: specify exactly one argument authorization mode"
                ));
            }
            let allowed_subcommands = command.allowed_subcommands.unwrap_or_default();
            if !command.allow_any_args
                && (allowed_subcommands.is_empty()
                    || allowed_subcommands.iter().any(|s| s.is_empty()))
            {
                return Err(format!("{name}: allowed_subcommands must be nonempty"));
            }
            commands.insert(
                name,
                CommandEntry {
                    path: executable(&command.executable)?,
                    allow_any_args: command.allow_any_args,
                    allowed_subcommands,
                },
            );
        }
        Ok(Self {
            roots,
            limits,
            environment: file.environment,
            commands,
        })
    }

    pub fn root_for<'a>(&'a self, path: &'a Path) -> Option<(&'a Root, &'a Path)> {
        if !path.is_absolute() {
            return None;
        }
        self.roots.iter().find_map(|root| {
            path.strip_prefix(&root.path)
                .or_else(|_| path.strip_prefix(&root.configured))
                .ok()
                .map(|relative| (root, relative))
        })
    }

    pub fn doctor(&self) -> Result<(), String> {
        for root in &self.roots {
            root.dir
                .read_dir(".")
                .map_err(|e| format!("{}: {e}", root.path.display()))?;
            #[cfg(unix)]
            {
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
            #[cfg(windows)]
            {
                let probe = format!(".rivet-doctor-{}", std::process::id());
                let mut options = cap_std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                let file = root.dir.open_with(&probe, &options).map_err(|error| {
                    format!("{}: root needs write access: {error}", root.path.display())
                })?;
                drop(file);
                root.dir.remove_file(&probe).map_err(|error| {
                    format!(
                        "{}: failed to remove write check: {error}",
                        root.path.display()
                    )
                })?;
            }
        }
        #[cfg(unix)]
        for fd in [0, 1, 2] {
            // SAFETY: fcntl only inspects the fixed stdio descriptor number.
            if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
                return Err(format!("stdio fd {fd} is unavailable"));
            }
        }
        Ok(())
    }
}
