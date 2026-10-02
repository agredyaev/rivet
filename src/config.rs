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
    pub filesystem: FilesystemConfig,
    pub limits: Limits,
    pub environment: Environment,
    pub commands: BTreeMap<String, CommandConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemConfig {
    #[serde(default)]
    pub allowed_roots: Vec<PathBuf>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    pub executable: String,
    #[serde(default)]
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
            if let Ok(canonical) = fs::canonicalize(path)
                && let Ok(meta) = fs::metadata(&canonical)
                && executable_file(&meta)
            {
                return Ok(canonical);
            }
        }
    }
    Err(format!(
        "executable {input:?} was not found or is not executable"
    ))
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
    pub fn load(path: &Path, cli_roots: Vec<PathBuf>) -> Result<Self, String> {
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
        let overridden = !cli_roots.is_empty();
        let configured_roots = if overridden {
            cli_roots
        } else {
            file.filesystem.allowed_roots
        };
        if configured_roots.is_empty() {
            return Err("allowed_roots must not be empty; set it in config or pass --root".into());
        }
        let mut roots = Vec::new();
        let mut seen = BTreeSet::new();
        for path in configured_roots {
            let path = if overridden && !path.is_absolute() {
                env::current_dir()
                    .map_err(|e| format!("failed to resolve --root: {e}"))?
                    .join(path)
            } else {
                path
            };
            if !path.is_absolute() {
                return Err(format!("root must be absolute: {}", path.display()));
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
        for (name, command) in file.commands {
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
