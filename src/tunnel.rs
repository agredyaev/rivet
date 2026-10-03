use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const RELEASE_API: &str = "https://api.github.com/repos/openai/tunnel-client/releases/latest";

struct ProfileCleanup(PathBuf);

impl Drop for ProfileCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn download(url: &str, path: &Path) -> Result<(), String> {
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--connect-timeout",
            "10",
            "--max-time",
            "300",
            url,
            "--output",
        ])
        .arg(path)
        .status()
        .map_err(|error| format!("Could not run curl: {error}"))?;
    if output.success() {
        Ok(())
    } else {
        Err(format!("Download failed: {url}"))
    }
}

fn platform() -> Result<&'static str, String> {
    Ok(match (env::consts::OS, env::consts::ARCH) {
        ("linux", "x86_64") => "linux-amd64",
        ("linux", "aarch64") => "linux-arm64",
        ("macos", "x86_64") => "darwin-amd64",
        ("macos", "aarch64") => "darwin-arm64",
        ("windows", "x86_64") => "windows-amd64",
        ("windows", "aarch64") => "windows-arm64",
        _ => return Err("Unsupported OpenAI tunnel-client target".into()),
    })
}

fn full_client_archive<'a>(
    assets: &'a [serde_json::Value],
    target: &str,
) -> Option<&'a serde_json::Value> {
    assets.iter().find(|asset| {
        asset["name"].as_str().is_some_and(|name| {
            name.starts_with("tunnel-client-")
                && name.ends_with(&format!("-{target}.zip"))
                && !name.contains("runtime")
        })
    })
}

fn install_client(bin: &Path) -> Result<PathBuf, String> {
    let client_name = if cfg!(windows) {
        "tunnel-client.exe"
    } else {
        "tunnel-client"
    };
    let client = bin.join(client_name);
    let cloudflared = bin.join(if cfg!(windows) {
        "cloudflared.exe"
    } else {
        "cloudflared"
    });
    println!("  Preparing OpenAI tunnel-client...");

    let api = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--connect-timeout",
            "5",
            "--max-time",
            "20",
            RELEASE_API,
        ])
        .output()
        .map_err(|error| format!("Could not check OpenAI tunnel-client releases: {error}"))?;
    if !api.status.success() {
        if client.is_file() && cloudflared.is_file() {
            println!("  ✓ GitHub is unavailable; using the installed tunnel-client.");
            return Ok(client);
        }
        return Err("Could not reach the OpenAI tunnel-client release on GitHub".into());
    }
    let release: serde_json::Value = serde_json::from_slice(&api.stdout)
        .map_err(|error| format!("Invalid OpenAI release response: {error}"))?;
    let tag = release["tag_name"]
        .as_str()
        .ok_or("Release tag is missing")?;
    if !tag.starts_with('v') || tag[1..].split('.').count() != 3 {
        return Err(format!("Unexpected OpenAI release tag: {tag}"));
    }
    if client.is_file()
        && cloudflared.is_file()
        && let Ok(installed) = Command::new(&client).arg("--version").output()
    {
        let version = tag.trim_start_matches('v');
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&installed.stdout),
            String::from_utf8_lossy(&installed.stderr)
        );
        if installed.status.success()
            && output
                .lines()
                .any(|line| line.trim().trim_start_matches('v').starts_with(version))
        {
            println!("  ✓ tunnel-client {tag} is already installed.");
            return Ok(client);
        }
    }
    let target = platform()?;
    let assets = release["assets"]
        .as_array()
        .ok_or("Release assets are missing")?;
    let archive = full_client_archive(assets, target)
        .ok_or_else(|| format!("No full tunnel-client archive for {target} in release {tag}"))?;
    let archive_name = archive["name"].as_str().ok_or("Archive name is missing")?;
    let archive_url = archive["browser_download_url"]
        .as_str()
        .ok_or("Archive URL is missing")?;
    let checksum = assets
        .iter()
        .find(|asset| asset["name"].as_str() == Some("SHA256SUMS.txt"))
        .and_then(|asset| asset["browser_download_url"].as_str())
        .ok_or("Official SHA256SUMS.txt is missing")?;

    fs::create_dir_all(bin).map_err(|error| error.to_string())?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = env::temp_dir().join(format!("rivet-tunnel-{}-{nonce}", std::process::id()));
    fs::create_dir(&temp).map_err(|error| error.to_string())?;
    let result = (|| {
        let archive_path = temp.join(archive_name);
        let sums_path = temp.join("SHA256SUMS.txt");
        download(archive_url, &archive_path)?;
        download(checksum, &sums_path)?;
        let sums = fs::read_to_string(&sums_path).map_err(|e| e.to_string())?;
        let expected = sums
            .lines()
            .find_map(|line| {
                let mut parts = line.split_whitespace();
                let hash = parts.next()?;
                let name = parts.next()?.trim_start_matches('*');
                (name == archive_name).then_some(hash.to_ascii_lowercase())
            })
            .ok_or_else(|| format!("Official checksum is missing for {archive_name}"))?;
        let bytes = fs::read(&archive_path).map_err(|e| e.to_string())?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual != expected {
            return Err(format!("SHA-256 verification failed for {archive_name}"));
        }

        let unpacked = temp.join("unpacked");
        fs::create_dir(&unpacked).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        {
            let script = "$ErrorActionPreference='Stop'; Expand-Archive -LiteralPath $env:RIVET_ARCHIVE -DestinationPath $env:RIVET_UNPACKED -Force";
            let status = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command", script])
                .env("RIVET_ARCHIVE", &archive_path)
                .env("RIVET_UNPACKED", &unpacked)
                .status()
                .map_err(|e| format!("Could not extract tunnel-client archive: {e}"))?;
            if !status.success() {
                return Err("Could not extract tunnel-client archive".into());
            }
        }
        #[cfg(not(windows))]
        {
            let status = Command::new("unzip")
                .arg("-q")
                .arg(&archive_path)
                .arg("-d")
                .arg(&unpacked)
                .status()
                .map_err(|error| format!("Could not run unzip: {error}"))?;
            if !status.success() {
                return Err(format!("unzip exited with {status}"));
            }
        }

        let extracted_client = unpacked.join(client_name);
        let extracted_cloudflared = unpacked.join(if cfg!(windows) {
            "cloudflared.exe"
        } else {
            "cloudflared"
        });
        if !extracted_client.is_file() || !extracted_cloudflared.is_file() {
            return Err("The official tunnel-client ZIP does not contain the expected client and cloudflared binaries".into());
        }
        fs::copy(extracted_client, &client).map_err(|e| e.to_string())?;
        fs::copy(extracted_cloudflared, &cloudflared).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&client, &cloudflared] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
            }
        }
        println!("  ✓ Installed and verified tunnel-client {tag} ({target}).");
        Ok(())
    })();
    let _ = fs::remove_dir_all(temp);
    result?;
    Ok(client)
}

fn input(prompt: &str) -> Result<String, String> {
    print!("  {prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .map_err(|error| format!("Could not read terminal input: {error}"))?;
    Ok(value.trim().to_owned())
}

fn valid_tunnel_id(value: &str) -> bool {
    value.starts_with("tunnel_")
        && value.len() == 39
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn secret(prompt: &str) -> Result<String, String> {
    print!("  {prompt}");
    io::stdout().flush().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        let echo = Command::new("stty")
            .arg("-echo")
            .status()
            .map_err(|error| format!("Could not disable terminal echo: {error}"))?;
        if !echo.success() {
            return Err("Could not disable terminal echo".into());
        }
        let mut value = String::new();
        let result = io::stdin().read_line(&mut value).map_err(|e| e.to_string());
        let restored = Command::new("stty").arg("echo").status();
        println!();
        if !restored.is_ok_and(|status| status.success()) {
            return Err("Could not restore terminal echo".into());
        }
        result?;
        Ok(value.trim().to_owned())
    }
    #[cfg(windows)]
    {
        let script = "$s=Read-Host -AsSecureString; $p=[Runtime.InteropServices.Marshal]::SecureStringToBSTR($s); try {[Runtime.InteropServices.Marshal]::PtrToStringBSTR($p)} finally {[Runtime.InteropServices.Marshal]::ZeroFreeBSTR($p); $s.Dispose()}";
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", script])
            .output()
            .map_err(|e| e.to_string())?;
        println!();
        if !output.status.success() {
            return Err("Could not read secret input".into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn run_with_timeout(command: &mut Command, label: &str, timeout: Duration) -> Result<(), String> {
    println!("  {label}...");
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start {label}: {error}"))?;
    let started = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|error| format!("Could not monitor {label}: {error}"))?
        {
            Some(status) if status.success() => {
                println!("  ✓ {label}.");
                return Ok(());
            }
            Some(status) => return Err(format!("{label} failed with {status}")),
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{label} timed out after {} seconds",
                    timeout.as_secs()
                ));
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

pub fn run(root: &Path, rivet: &Path, config: &Path, args: &[String]) -> ExitCode {
    if !io::stdin().is_terminal() {
        eprintln!("Rivet session needs an interactive terminal.");
        return ExitCode::FAILURE;
    }
    let status = (|| -> Result<(), String> {
        for action in ["config-check", "doctor"] {
            let check = Command::new(rivet)
                .args([action, "--config"])
                .arg(config)
                .args(args)
                .status()
                .map_err(|error| format!("Could not run Rivet {action}: {error}"))?;
            if !check.success() {
                return Err(format!("Rivet {action} failed"));
            }
        }
        let tunnel = install_client(&root.join("bin"))?;
        println!("\n  OpenAI tunnel: https://platform.openai.com/settings/organization/tunnels");
        let tunnel_id = input("Tunnel ID: ")?;
        if !valid_tunnel_id(&tunnel_id) {
            return Err("Expected tunnel_ followed by 32 hexadecimal characters".into());
        }
        println!("  ✓ Tunnel ID accepted.");

        let profile_dir = root.join("tunnel-client-profiles");
        fs::create_dir_all(&profile_dir).map_err(|e| e.to_string())?;
        let _profile_cleanup = ProfileCleanup(profile_dir.join("rivet.yaml"));
        let mcp = format!(
            "{} session-mcp --config {} {}",
            quote(rivet.to_str().ok_or("Invalid Rivet executable path")?),
            quote(config.to_str().ok_or("Invalid config path")?),
            args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
        );
        let mut init = Command::new(&tunnel);
        init.args([
            "init",
            "--force",
            "--sample",
            "sample_mcp_stdio_local",
            "--profile",
            "rivet",
            "--tunnel-id",
            &tunnel_id,
            "--mcp-command",
            &mcp,
            "--control-plane-api-key-ref",
            "env:CONTROL_PLANE_API_KEY",
        ])
        .env("TUNNEL_CLIENT_PROFILE_DIR", &profile_dir);
        run_with_timeout(
            &mut init,
            "Creating temporary tunnel profile",
            Duration::from_secs(30),
        )?;
        let key = secret("Runtime API key (input hidden): ")?;
        if key.is_empty() {
            return Err("Runtime API key cannot be empty".into());
        }
        let mut doctor = Command::new(&tunnel);
        doctor
            .args(["doctor", "--profile", "rivet", "--explain"])
            .env("TUNNEL_CLIENT_PROFILE_DIR", &profile_dir)
            .env("CONTROL_PLANE_API_KEY", &key);
        run_with_timeout(
            &mut doctor,
            "Checking tunnel credentials and configuration",
            Duration::from_secs(60),
        )?;

        println!("  Starting tunnel-client...");
        let mut tunnel_process = Command::new(&tunnel)
            .args(["run", "--profile", "rivet"])
            .env("TUNNEL_CLIENT_PROFILE_DIR", &profile_dir)
            .env("CONTROL_PLANE_API_KEY", &key)
            .spawn()
            .map_err(|error| format!("Could not start tunnel-client: {error}"))?;
        println!("\n  ✓ tunnel-client started. Keep this terminal open; press Ctrl+C to stop it.");
        let status = tunnel_process
            .wait()
            .map_err(|error| format!("Could not wait for tunnel-client: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("tunnel-client exited with {status}"))
        }
    })();
    match status {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("\n  ✗ {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_tunnel_id_format() {
        assert!(valid_tunnel_id("tunnel_0123456789abcdef0123456789abcdef"));
        assert!(!valid_tunnel_id("tunnel_0123456789abcdef"));
        assert!(!valid_tunnel_id("tunnel_0123456789abcdef0123456789abcdeg"));
        assert!(!valid_tunnel_id("other__0123456789abcdef0123456789abcdef"));
    }

    #[test]
    fn selects_full_client_for_detected_target() {
        let assets = [
            serde_json::json!({"name": "tunnel-client-runtime-cloudflared-v0.0.15-darwin-arm64.zip"}),
            serde_json::json!({"name": "tunnel-client-v0.0.15-darwin-amd64.zip"}),
            serde_json::json!({"name": "tunnel-client-v0.0.15-darwin-arm64.zip"}),
        ];

        assert_eq!(
            full_client_archive(&assets, "darwin-arm64").unwrap()["name"],
            "tunnel-client-v0.0.15-darwin-arm64.zip"
        );
        assert!(full_client_archive(&assets, "linux-amd64").is_none());
    }
}
