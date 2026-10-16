use crate::config::Config;
use std::{
    collections::BTreeMap,
    env,
    io::{self, BufRead, IsTerminal, Write},
    path::Path,
    process::ExitCode,
    sync::Arc,
};

const BUILT_INS: [(&str, &str); 6] = [
    ("uv", "uv"),
    ("mkdir", "mkdir"),
    ("rg", "rg"),
    ("git", "git"),
    ("cargo", "cargo"),
    ("make", "make"),
];

fn read_line<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    prompt: &str,
) -> io::Result<String> {
    write!(output, "{prompt}")?;
    output.flush()?;
    let mut line = String::new();
    if input.read_line(&mut line)? == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Session setup cancelled: input closed",
        ));
    }
    Ok(line.trim().to_owned())
}

fn confirm<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    prompt: &str,
) -> Result<bool, String> {
    loop {
        let value = read_line(input, output, prompt).map_err(|error| error.to_string())?;
        match value.to_ascii_lowercase().as_str() {
            "" | "n" | "no" => return Ok(false),
            "y" | "yes" => return Ok(true),
            _ => writeln!(output, "Enter y or n.").map_err(|error| error.to_string())?,
        }
    }
}

fn prompt_executable<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    mut executable: String,
) -> Result<String, String> {
    loop {
        match crate::config::executable(&executable) {
            Ok(_) => return Ok(executable),
            Err(error) => writeln!(output, "{error}").map_err(|error| error.to_string())?,
        }
        executable = read_line(input, output, "Executable or path (replacement): ")
            .map_err(|error| error.to_string())?;
    }
}

fn collect_commands<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    cwd: &Path,
    color: bool,
) -> Result<BTreeMap<String, String>, String> {
    let heading = if color {
        "\x1b[1;36mWorkspace\x1b[0m"
    } else {
        "Workspace"
    };
    writeln!(output, "\n{heading}: {}", cwd.display()).map_err(|error| error.to_string())?;
    writeln!(output, "\nBuilt-in commands:").map_err(|error| error.to_string())?;
    for (index, (name, _)) in BUILT_INS.iter().enumerate() {
        writeln!(output, "  {}. {name}", index + 1).map_err(|error| error.to_string())?;
    }
    writeln!(output, "  7. Add a custom command").map_err(|error| error.to_string())?;
    writeln!(
        output,
        "  Select multiple items with commas (example: 1,3,7). Enter selects none."
    )
    .map_err(|error| error.to_string())?;

    let mut commands = BTreeMap::new();
    let add_custom;
    loop {
        let selection =
            read_line(input, output, "Select items: ").map_err(|error| error.to_string())?;
        let mut chosen = Vec::new();
        let mut invalid = false;
        for item in selection.split(',').filter(|item| !item.trim().is_empty()) {
            match item.trim().parse::<usize>() {
                Ok(number)
                    if (1..=BUILT_INS.len() + 1).contains(&number)
                        && !chosen.contains(&(number - 1)) =>
                {
                    chosen.push(number - 1);
                }
                _ => {
                    invalid = true;
                    break;
                }
            }
        }
        if invalid {
            writeln!(
                output,
                "Enter distinct numbers from 1 to {}.",
                BUILT_INS.len() + 1
            )
            .map_err(|error| error.to_string())?;
            continue;
        }

        add_custom = chosen.contains(&BUILT_INS.len());
        for index in chosen.into_iter().filter(|index| *index < BUILT_INS.len()) {
            let (name, executable) = BUILT_INS[index];
            commands.insert(
                name.to_owned(),
                prompt_executable(input, output, executable.to_owned())?,
            );
        }
        break;
    }

    if add_custom {
        loop {
            let name = read_line(input, output, "Command name (the name Rivet exposes): ")
                .map_err(|error| error.to_string())?;
            let executable = read_line(input, output, "Executable or path: ")
                .map_err(|error| error.to_string())?;
            if name.is_empty()
                || executable.is_empty()
                || name.chars().any(char::is_whitespace)
                || name.contains('=')
                || commands.contains_key(&name)
            {
                writeln!(
                    output,
                    "Name and executable must be nonempty; names cannot contain spaces, '=', or repeat."
                )
                .map_err(|error| error.to_string())?;
                continue;
            }

            commands.insert(name, prompt_executable(input, output, executable)?);
            if !confirm(input, output, "Add another custom command? [y/N]: ")? {
                break;
            }
        }
    }

    Ok(commands)
}

fn collect_environment<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
) -> Result<Vec<String>, String> {
    if !confirm(
        input,
        output,
        "\nAllow child commands to access specific environment variables (for API keys or tokens)? [y/N]: ",
    )? {
        return Ok(Vec::new());
    }

    loop {
        let value = read_line(
            input,
            output,
            "Environment variable names (comma-separated, values are never displayed): ",
        )
        .map_err(|error| error.to_string())?;
        let names: Vec<String> = value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        if !names.is_empty() {
            return Ok(names);
        }
        writeln!(output, "Enter at least one environment variable name.")
            .map_err(|error| error.to_string())?;
    }
}

fn print_scope<W: Write>(output: &mut W, config: &Config, color: bool) -> io::Result<()> {
    let check = if color { "\x1b[32m✓\x1b[0m" } else { "✓" };
    writeln!(output, "\nSession scope:")?;
    writeln!(
        output,
        "  {check} Workspace: {}",
        config.root.path.display()
    )?;
    writeln!(
        output,
        "  {check} Child network: {}",
        if config.network {
            "enabled"
        } else {
            "disabled"
        }
    )?;
    if config.pass_env.is_empty() {
        writeln!(output, "  {check} Environment passthrough: none")?;
    } else {
        let names = config
            .pass_env
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(output, "  {check} Environment passthrough: {names}")?;
    }
    if config.commands.is_empty() {
        writeln!(output, "  {check} Commands: none")?;
    } else {
        for command in &config.commands {
            writeln!(
                output,
                "  {check} {} → {}",
                command.name,
                command.path.display()
            )?;
        }
    }
    Ok(())
}

type ParsedArgs = (BTreeMap<String, String>, Vec<String>, bool);

fn parse_args(args: Vec<String>) -> Result<ParsedArgs, String> {
    let mut commands = BTreeMap::new();
    let mut pass_env = Vec::new();
    let mut network = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--network" => network = true,
            "--allow-command" => {
                let value = iter.next().ok_or("--allow-command requires NAME=VALUE")?;
                let (name, executable) = crate::assignment("--allow-command", &value)?;
                if commands
                    .insert(name.to_owned(), executable.to_owned())
                    .is_some()
                {
                    return Err(format!("duplicate --allow-command for {name}"));
                }
            }
            "--pass-env" => {
                pass_env.push(iter.next().ok_or("--pass-env requires NAME")?);
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }

    Ok((commands, pass_env, network))
}

pub async fn run(args: Vec<String>) -> ExitCode {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        crate::print_usage(Some("session"));
        return ExitCode::SUCCESS;
    }

    let (mut commands, mut pass_env, mut network) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    let cwd = match env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("Cannot read current directory: {error}");
            return ExitCode::FAILURE;
        }
    };

    let color = io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    if commands.is_empty() {
        let mut input = io::stdin().lock();
        let mut output = io::stdout().lock();
        commands = match collect_commands(&mut input, &mut output, &cwd, color) {
            Ok(commands) => commands,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };

        if !network {
            network = match confirm(
                &mut input,
                &mut output,
                "\nAllow child network access (needed for package or dependency downloads)? [y/N]: ",
            ) {
                Ok(network) => network,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::FAILURE;
                }
            };
        }

        if pass_env.is_empty() {
            pass_env = match collect_environment(&mut input, &mut output) {
                Ok(names) => names,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::FAILURE;
                }
            };
        }
    }

    let config = match Config::new(cwd, commands, pass_env, network) {
        Ok(config) => Arc::new(config),
        Err(error) => {
            eprintln!("STARTUP_ERROR: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = crate::sandbox::doctor() {
        eprintln!("SANDBOX_ERROR: {error}");
        return ExitCode::FAILURE;
    }

    if let Err(error) = print_scope(&mut io::stdout().lock(), &config, color) {
        eprintln!("Cannot print session scope: {error}");
        return ExitCode::FAILURE;
    }

    match crate::tunnel::run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn selection_builds_builtin_and_custom_commands() {
        let cwd = env::current_dir().unwrap();
        let mut input = Cursor::new("2,4,5,7\nhttp\ncurl\ny\nhello\nsh\nn\n");
        let mut output = Vec::new();
        let commands = collect_commands(&mut input, &mut output, &cwd, false).unwrap();

        assert_eq!(commands.get("mkdir").map(String::as_str), Some("mkdir"));
        assert_eq!(commands.get("git").map(String::as_str), Some("git"));
        assert_eq!(commands.get("cargo").map(String::as_str), Some("cargo"));
        assert_eq!(commands.get("http").map(String::as_str), Some("curl"));
        assert_eq!(commands.get("hello").map(String::as_str), Some("sh"));
        assert!(!commands.contains_key("uv"));
        assert!(!commands.contains_key("rg"));
        assert!(!commands.contains_key("make"));

        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains("7. Add a custom command"));
        assert!(!rendered.contains("first argument"));
    }

    #[test]
    fn interactive_capabilities_collect_network_and_env_names() {
        let mut input = Cursor::new("y\ny\nFRED_API_KEY, OTHER_TOKEN\n");
        let mut output = Vec::new();
        let network = confirm(
            &mut input,
            &mut output,
            "Allow child network access? [y/N]: ",
        )
        .unwrap();
        let pass_env = collect_environment(&mut input, &mut output).unwrap();
        assert!(network);
        assert_eq!(pass_env, ["FRED_API_KEY", "OTHER_TOKEN"]);
        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains("values are never displayed"));
    }

    #[test]
    fn args_have_one_command_policy_and_explicit_env_names() {
        let (commands, pass_env, network) = parse_args(vec![
            "--network".into(),
            "--allow-command".into(),
            "shell=/bin/sh".into(),
            "--pass-env".into(),
            "TOKEN".into(),
        ])
        .unwrap();
        assert_eq!(commands.get("shell").map(String::as_str), Some("/bin/sh"));
        assert_eq!(pass_env, ["TOKEN"]);
        assert!(network);

        let error = parse_args(vec!["--allow-subcommand".into(), "shell=-c".into()])
            .err()
            .unwrap();
        assert!(error.contains("unknown argument"));
    }
    #[test]
    fn summary_prints_env_names_but_not_values() {
        let name = format!("RIVET_TEST_SECRET_{}", std::process::id());
        let secret = "secret-value-must-not-render";
        unsafe { std::env::set_var(&name, secret) };

        let config = Config::new(
            env::current_dir().unwrap(),
            BTreeMap::from([("shell".into(), "/bin/sh".into())]),
            vec![name.clone()],
            false,
        )
        .unwrap();
        let mut output = Vec::new();
        print_scope(&mut output, &config, false).unwrap();
        let rendered = String::from_utf8(output).unwrap();

        assert!(rendered.contains(&name));
        assert!(!rendered.contains(secret));

        unsafe { std::env::remove_var(&name) };
    }
}
