//! Embedded defaults and startup scope validation.
//!
//! Session capabilities are resolved once at startup. Runtime state is stored
//! in flat parallel arrays and addressed by dense command IDs.
use cap_std::{ambient_authority, fs::Dir};
use std::{
    env,
    ffi::{CString, OsString},
    fs,
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
    sync::OnceLock,
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

pub const COMMAND_ID_ZERO: CommandId = CommandId(0);

#[inline]
pub const fn command_index(id: CommandId) -> usize {
    id.0 as usize
}

static SESSION_CONFIG: OnceLock<Config> = OnceLock::new();

pub struct Config {
    pub root: Root,
    pub command_name: Vec<String>,
    pub command_executable: Vec<PathBuf>,
    pub env_name: Vec<String>,
    pub env_value: Vec<OsString>,
    pub command_path_grant: Vec<PathBuf>,
    pub runtime_path: Option<OsString>,
    pub network: bool,
}

fn env_name_valid(name: &str) -> bool {
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

pub fn install(config: Config) -> Result<(), String> {
    SESSION_CONFIG
        .set(config)
        .map_err(|_| "session config is already installed".to_owned())
}

pub fn session() -> &'static Config {
    SESSION_CONFIG
        .get()
        .expect("session config must be installed before serving MCP")
}

#[cfg(test)]
pub fn test_session() -> &'static Config {
    SESSION_CONFIG.get_or_init(|| {
        build_test_config(
            env::current_dir().expect("test current directory"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
        )
        .expect("test session config")
    })
}

fn sort_commands(command_name: &mut [String], command_executable: &mut [PathBuf]) {
    for right in 1..command_name.len() {
        let mut index = right;
        while index > 0 && command_name[index] < command_name[index - 1] {
            command_name.swap(index, index - 1);
            command_executable.swap(index, index - 1);
            index -= 1;
        }
    }
}

#[cfg(test)]
pub fn build_test_config(
    workspace: PathBuf,
    command_name: Vec<String>,
    command_executable: Vec<String>,
    pass_env_names: Vec<String>,
    network: bool,
) -> Result<Config, String> {
    build_config(
        workspace,
        command_name,
        command_executable,
        pass_env_names,
        Vec::new(),
        network,
    )
}

pub fn build_config(
    workspace: PathBuf,
    mut command_name: Vec<String>,
    command_executable_spec: Vec<String>,
    pass_env_names: Vec<String>,
    command_path_specs: Vec<PathBuf>,
    network: bool,
) -> Result<Config, String> {
    let root = root(workspace)?;

    let mut command_path_grant = Vec::with_capacity(command_path_specs.len());
    for path in command_path_specs {
        if !path.is_absolute() {
            return Err(format!(
                "--allow-command-path requires an absolute path: {}",
                path.display()
            ));
        }
        let canonical =
            fs::canonicalize(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        command_path_grant.push(canonical);
    }
    command_path_grant.sort_unstable();
    command_path_grant.dedup();

    if command_name.len() != command_executable_spec.len() {
        return Err("command name/executable arrays have different lengths".into());
    }
    if command_name.len() > u16::MAX as usize + 1 {
        return Err("too many registered commands".into());
    }

    let mut command_executable = Vec::with_capacity(command_name.len());
    for index in 0..command_name.len() {
        let name = &command_name[index];
        if name.is_empty() || name.contains(char::is_whitespace) || name.contains('=') {
            return Err(format!("invalid command name: {name:?}"));
        }
        command_executable.push(executable(&command_executable_spec[index])?);
    }
    sort_commands(&mut command_name, &mut command_executable);
    for index in 1..command_name.len() {
        if command_name[index - 1] == command_name[index] {
            return Err(format!(
                "duplicate --allow-command for {}",
                command_name[index]
            ));
        }
    }

    let mut env_name = Vec::with_capacity(pass_env_names.len());
    let mut env_value = Vec::with_capacity(pass_env_names.len());
    for name in pass_env_names {
        if !env_name_valid(&name) {
            return Err(format!("invalid environment name: {name:?}"));
        }
        if reserved_env(&name) {
            return Err(format!("{name}: environment name is reserved by Rivet"));
        }
        if env_name.iter().any(|existing| existing == &name) {
            return Err(format!("duplicate --pass-env {name}"));
        }
        let value = env::var_os(&name)
            .ok_or_else(|| format!("{name}: parent environment variable is not set"))?;
        env_name.push(name);
        env_value.push(value);
    }

    Ok(Config {
        root,
        command_name,
        command_executable,
        env_name,
        env_value,
        command_path_grant,
        runtime_path: env::var_os("PATH"),
        network,
    })
}

pub fn command_id(config: &Config, name: &str) -> Option<CommandId> {
    let index = config
        .command_name
        .binary_search_by(|entry| entry.as_str().cmp(name))
        .ok()?;
    Some(CommandId(index as u16))
}

#[inline]
pub fn command_name(config: &Config, id: CommandId) -> &str {
    &config.command_name[command_index(id)]
}

#[inline]
pub fn command_executable(config: &Config, id: CommandId) -> &Path {
    &config.command_executable[command_index(id)]
}

pub fn root_for<'a>(config: &'a Config, path: &'a Path) -> Option<(&'a Root, &'a Path)> {
    if !path.is_absolute() {
        return None;
    }
    rivet::path_policy::relative_under(&config.root.path, path)
        .map(|relative| (&config.root, relative))
}

#[cfg(all(test, unix))]
mod tests {
    use super::{build_config, build_test_config, command_id, command_name, executable, root_for};
    use std::{
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
    fn session_state_is_sorted_parallel_command_arrays() {
        let root = temp_root("session");
        let config = build_test_config(
            root.clone(),
            vec!["zeta".into(), "alpha".into()],
            vec!["/bin/echo".into(), "/bin/echo".into()],
            Vec::new(),
            false,
        )
        .unwrap();

        assert_eq!(config.root.path, root.canonicalize().unwrap());
        assert_eq!(config.command_name, ["alpha", "zeta"]);
        assert_eq!(config.command_name.len(), config.command_executable.len());

        let alpha = command_id(&config, "alpha").unwrap();
        let zeta = command_id(&config, "zeta").unwrap();
        assert_eq!(command_name(&config, alpha), "alpha");
        assert_eq!(command_name(&config, zeta), "zeta");
        assert_ne!(alpha.0, zeta.0);
        assert!(!config.network);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn command_paths_are_canonicalized_sorted_and_do_not_expand_workspace_root() {
        let root = temp_root("command-path-workspace");
        let extra_a = temp_root("command-path-a");
        let extra_b = temp_root("command-path-b");

        let config = build_config(
            root.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![extra_b.clone(), extra_a.clone(), extra_a.clone()],
            false,
        )
        .unwrap();

        assert_eq!(
            config.command_path_grant,
            [
                extra_a.canonicalize().unwrap(),
                extra_b.canonicalize().unwrap()
            ]
        );
        assert!(root_for(&config, &extra_a.join("outside-workspace")).is_none());

        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(extra_a).unwrap();
        fs::remove_dir_all(extra_b).unwrap();
    }

    #[test]
    fn command_paths_reject_relative_and_missing_paths() {
        let root = temp_root("command-path-invalid");

        let relative = build_config(
            root.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![std::path::PathBuf::from("relative")],
            false,
        )
        .err()
        .unwrap();
        assert!(relative.contains("absolute"));

        let missing_path = root.join("missing");
        let missing = build_config(
            root.clone(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![missing_path.clone()],
            false,
        )
        .err()
        .unwrap();
        assert!(missing.contains(&missing_path.display().to_string()));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pass_env_rejects_reserved_and_missing_names() {
        let root = temp_root("env");
        let reserved = build_test_config(
            root.clone(),
            Vec::new(),
            Vec::new(),
            vec!["HOME".into()],
            false,
        )
        .err()
        .unwrap();
        assert!(reserved.contains("reserved"));

        let missing_name = format!("RIVET_TEST_MISSING_{}", std::process::id());
        unsafe { std::env::remove_var(&missing_name) };
        let missing = build_test_config(
            root.clone(),
            Vec::new(),
            Vec::new(),
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

        let config = build_test_config(
            root.clone(),
            Vec::new(),
            Vec::new(),
            vec![name.clone()],
            false,
        )
        .unwrap();
        unsafe { std::env::set_var(&name, "second") };

        assert_eq!(config.env_name.as_slice(), std::slice::from_ref(&name));
        assert_eq!(config.env_value, [std::ffi::OsString::from("first")]);

        let duplicate = build_test_config(
            root.clone(),
            Vec::new(),
            Vec::new(),
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
