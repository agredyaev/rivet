//! Embedded defaults and startup scope validation.
//!
//! Session capabilities are resolved once at startup. The runtime retains one
//! validated workspace root, resolved executables, and explicit environment
//! values selected by the operator.
use cap_std::{ambient_authority, fs::Dir};
use std::{
    collections::BTreeMap,
    env,
    ffi::{CString, OsString},
    fs,
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
};

pub const PROCESS_RETENTION_SLOTS: usize = 32;
pub const MAX_RUNNING_PROCESSES: usize = 8;
pub const MAX_PROCESS_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DIRECTORY_ENTRIES: usize = 1000;
pub const DEFAULT_TIMEOUT_MS: u64 = 120_000;
pub const MAX_TIMEOUT_MS: u64 = 1_800_000;

const _: () = {
    assert!(PROCESS_RETENTION_SLOTS > 0);
    assert!(MAX_RUNNING_PROCESSES > 0 && MAX_RUNNING_PROCESSES <= PROCESS_RETENTION_SLOTS);
    assert!(MAX_PROCESS_OUTPUT_BYTES > 0);
    assert!(MAX_TEXT_BYTES > 0 && MAX_TEXT_BYTES <= 64 * 1024 * 1024);
    assert!(MAX_DIRECTORY_ENTRIES > 0);
    assert!(DEFAULT_TIMEOUT_MS > 0);
    assert!(MAX_TIMEOUT_MS > 0 && MAX_TIMEOUT_MS <= u32::MAX as u64);
    assert!(DEFAULT_TIMEOUT_MS <= MAX_TIMEOUT_MS);
};

pub struct Root {
    pub path: PathBuf,
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
}

pub struct Config {
    pub root: Root,
    pub commands: Box<[CommandEntry]>,
    pub pass_env: Box<[(String, OsString)]>,
    pub runtime_path: Option<OsString>,
    pub network: bool,
}

fn env_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['=', '\0'])
}

fn reserved_env(name: &str) -> bool {
    matches!(
        name,
        "PATH" | "HOME" | "TMPDIR" | "CARGO_HOME" | "RUSTUP_HOME" | "DEVELOPER_DIR" | "SDKROOT"
    )
}

pub(crate) fn executable(input: &str) -> Result<PathBuf, String> {
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
            // Keep the resolved path's final component: tools such as rustup
            // select proxy behavior from argv[0] (for example, cargo).
            return Ok(path);
        }
    }
    Err(format!(
        "executable {input:?} was not found or is not executable"
    ))
}

fn executable_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

fn root(path: PathBuf) -> Result<Root, String> {
    let canonical =
        fs::canonicalize(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "workspace is not a directory: {}",
            canonical.display()
        ));
    }

    let dir = Dir::open_ambient_dir(&canonical, ambient_authority())
        .map_err(|error| format!("{}: {error}", canonical.display()))?;
    dir.read_dir(".")
        .map_err(|error| format!("{}: {error}", canonical.display()))?;

    let dot = CString::new(".").expect("static string");
    // SAFETY: the directory fd is open and dot is a valid NUL-terminated path.
    if unsafe {
        libc::faccessat(
            dir.as_raw_fd(),
            dot.as_ptr(),
            libc::R_OK | libc::W_OK | libc::X_OK,
            0,
        )
    } < 0
    {
        return Err(format!(
            "{}: workspace needs read, write, and search access",
            canonical.display()
        ));
    }

    Ok(Root {
        path: canonical,
        dir,
    })
}

impl Config {
    pub fn new(
        workspace: PathBuf,
        command_specs: BTreeMap<String, String>,
        pass_env_names: Vec<String>,
        network: bool,
    ) -> Result<Self, String> {
        let root = root(workspace)?;

        if command_specs.len() > u16::MAX as usize + 1 {
            return Err("too many registered commands".into());
        }
        let mut commands = Vec::with_capacity(command_specs.len());
        for (name, executable_name) in command_specs {
            if name.is_empty() || name.contains(char::is_whitespace) || name.contains('=') {
                return Err(format!("invalid command name: {name:?}"));
            }
            commands.push(CommandEntry {
                name,
                path: executable(&executable_name)?,
            });
        }

        let mut pass_env = Vec::with_capacity(pass_env_names.len());
        for name in pass_env_names {
            if !env_name(&name) {
                return Err(format!("invalid environment name: {name:?}"));
            }
            if reserved_env(&name) {
                return Err(format!("{name}: environment name is reserved by Rivet"));
            }
            if pass_env.iter().any(|(existing, _)| existing == &name) {
                return Err(format!("duplicate --pass-env {name}"));
            }
            let value = env::var_os(&name)
                .ok_or_else(|| format!("{name}: parent environment variable is not set"))?;
            pass_env.push((name, value));
        }

        Ok(Self {
            root,
            commands: commands.into_boxed_slice(),
            pass_env: pass_env.into_boxed_slice(),
            runtime_path: env::var_os("PATH"),
            network,
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
        rivet::path_policy::relative_under(&self.root.path, path)
            .map(|relative| (&self.root, relative))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{Config, executable};
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_root(label: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("rivet-{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&root).unwrap();
        root
    }

    #[test]
    fn executable_preserves_symlink_name_when_launched() {
        let dir = temp_root("executable");
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

    #[test]
    fn session_state_is_single_root_and_sorted_commands() {
        let root = temp_root("session");
        let mut commands = BTreeMap::new();
        commands.insert("zeta".into(), "/bin/echo".into());
        commands.insert("alpha".into(), "/bin/echo".into());

        let config = Config::new(root.clone(), commands, Vec::new(), false).unwrap();
        assert_eq!(config.root.path, root.canonicalize().unwrap());
        assert_eq!(
            config
                .commands
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        let (alpha, _) = config.command("alpha").unwrap();
        let (zeta, _) = config.command("zeta").unwrap();
        assert_eq!(config.command_name(alpha), "alpha");
        assert_eq!(config.command_name(zeta), "zeta");
        assert_ne!(alpha.0, zeta.0);
        assert!(!config.network);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pass_env_rejects_reserved_and_missing_names() {
        let root = temp_root("env");
        let reserved = Config::new(root.clone(), BTreeMap::new(), vec!["HOME".into()], false)
            .err()
            .unwrap();
        assert!(reserved.contains("reserved"));

        let missing_name = format!("RIVET_TEST_MISSING_{}", std::process::id());
        unsafe { std::env::remove_var(&missing_name) };
        let missing = Config::new(
            root.clone(),
            BTreeMap::new(),
            vec![missing_name.clone()],
            false,
        )
        .err()
        .unwrap();
        assert!(missing.contains(&missing_name));
        assert!(missing.contains("not set"));

        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn pass_env_is_captured_once_and_duplicates_fail() {
        let root = temp_root("env-snapshot");
        let name = format!("RIVET_TEST_ENV_{}", std::process::id());
        unsafe { std::env::set_var(&name, "first") };

        let config = Config::new(root.clone(), BTreeMap::new(), vec![name.clone()], false).unwrap();
        unsafe { std::env::set_var(&name, "second") };

        assert_eq!(config.pass_env.len(), 1);
        assert_eq!(config.pass_env[0].0, name);
        assert_eq!(config.pass_env[0].1, std::ffi::OsString::from("first"));

        let duplicate = Config::new(
            root.clone(),
            BTreeMap::new(),
            vec![name.clone(), name.clone()],
            false,
        )
        .err()
        .unwrap();
        assert!(duplicate.contains("duplicate --pass-env"));

        unsafe { std::env::remove_var(&name) };
        fs::remove_dir_all(root).unwrap();
    }
}
