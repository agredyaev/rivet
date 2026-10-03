use std::{
    env,
    io::{self, BufRead, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

struct Scope {
    roots: Vec<PathBuf>,
    commands: Vec<(String, String, Option<Vec<String>>)>,
}

const BUILT_INS: [(&str, &str, Option<&str>); 6] = [
    ("uv", "uv", None),
    ("mkdir", "mkdir", None),
    ("rg", "rg", None),
    (
        "git",
        "git",
        Some("status,diff,log,show,add,commit,rev-parse,ls-files"),
    ),
    ("cargo", "cargo", Some("check,test,fmt,clippy,metadata")),
    ("make", "make", None),
];

fn read_line<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    prompt: &str,
) -> io::Result<String> {
    write!(output, "{prompt}")?;
    output.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim().to_owned())
}

fn collect_scope<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    cwd: &Path,
    preset_roots: &[PathBuf],
    prompt_commands: bool,
    color: bool,
) -> Result<Scope, String> {
    let heading = if color {
        "\x1b[1;36mWorkspace\x1b[0m"
    } else {
        "Workspace"
    };
    let check = if color { "\x1b[32m✓\x1b[0m" } else { "✓" };
    writeln!(output, "\n{heading}: {}", cwd.display()).map_err(|error| error.to_string())?;
    let input_roots = if preset_roots.is_empty() {
        let root = read_line(
            input,
            output,
            "Workspace directory (Enter keeps this directory): ",
        )
        .map_err(|error| error.to_string())?;
        vec![if root.is_empty() {
            cwd.to_path_buf()
        } else {
            PathBuf::from(root)
        }]
    } else {
        preset_roots.to_vec()
    };
    let mut roots = Vec::new();
    for root in input_roots {
        let root = root
            .canonicalize()
            .map_err(|error| format!("Cannot use workspace {}: {error}", root.display()))?;
        if !root.is_dir() {
            return Err(format!("Workspace is not a directory: {}", root.display()));
        }
        roots.push(root);
    }

    let mut commands = Vec::new();
    if prompt_commands {
        writeln!(output, "\nBuilt-in commands:").map_err(|error| error.to_string())?;
        for (index, (name, _, access)) in BUILT_INS.iter().enumerate() {
            let permissions = access
                .map(|items| format!("first argument: {items}"))
                .unwrap_or_else(|| "any arguments".to_owned());
            writeln!(output, "  {}. {name} ({permissions})", index + 1)
                .map_err(|error| error.to_string())?;
        }
        writeln!(output, "  7. Add a custom command").map_err(|error| error.to_string())?;
        writeln!(
            output,
            "  Select multiple items with commas (example: 1,3,7). Enter selects none."
        )
        .map_err(|error| error.to_string())?;

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
                        chosen.push(number - 1)
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
                let (name, executable, access) = BUILT_INS[index];
                let allowed = access.map(|items| items.split(',').map(str::to_owned).collect());
                commands.push((name.to_owned(), executable.to_owned(), allowed));
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
                    || commands.iter().any(|(existing, _, _)| existing == &name)
                {
                    writeln!(
                    output,
                "Name and executable must be nonempty; names cannot contain spaces, '=', or repeat."
                )
                .map_err(|error| error.to_string())?;
                    continue;
                }
                let allowed = loop {
                    let value = read_line(
                        input,
                        output,
                        "Allowed first arguments (comma-separated, or * for any): ",
                    )
                    .map_err(|error| error.to_string())?;
                    if value == "*" {
                        break None;
                    }
                    let items: Vec<_> = value
                        .split(',')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_owned)
                        .collect();
                    if !items.is_empty() && items.len() == value.split(',').count() {
                        break Some(items);
                    }
                    writeln!(output, "Enter one or more arguments, or *.")
                        .map_err(|error| error.to_string())?;
                };
                commands.push((name, executable, allowed));
                let add_another = read_line(input, output, "Add another custom command? [y/N]: ")
                    .map_err(|error| error.to_string())?;
                if !matches!(add_another.to_ascii_lowercase().as_str(), "y" | "yes") {
                    break;
                }
            }
        }
    } else {
        writeln!(
            output,
            "\nCommand scope was supplied with command-line arguments."
        )
        .map_err(|error| error.to_string())?;
    }

    writeln!(output, "\nSession scope:").map_err(|error| error.to_string())?;
    for root in &roots {
        writeln!(output, "  {check} Workspace: {}", root.display())
            .map_err(|error| error.to_string())?;
    }
    if commands.is_empty() {
        writeln!(output, "  {check} Commands: none").map_err(|error| error.to_string())?;
    } else {
        for (name, executable, allowed) in &commands {
            let access = allowed
                .as_ref()
                .map(|values| values.join(", "))
                .unwrap_or_else(|| "any arguments".to_owned());
            writeln!(output, "  {check} {name} → {executable} ({access})")
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(Scope { roots, commands })
}

fn scope_args(scope: &Scope) -> Vec<String> {
    let mut args = Vec::new();
    for root in &scope.roots {
        args.extend(["--root".into(), root.display().to_string()]);
    }
    for (name, executable, allowed) in &scope.commands {
        args.extend(["--allow-command".into(), format!("{name}={executable}")]);
        match allowed {
            Some(values) => {
                for value in values {
                    args.extend(["--allow-subcommand".into(), format!("{name}={value}")]);
                }
            }
            None => args.extend(["--allow-any-args".into(), name.clone()]),
        }
    }
    args
}

fn package_root() -> Result<PathBuf, String> {
    let executable = env::current_exe().map_err(|error| error.to_string())?;
    let root = executable
        .parent()
        .and_then(Path::parent)
        .ok_or("Cannot locate Rivet package directory")?;
    if root.join("rivet.toml").is_file() {
        return Ok(root.to_path_buf());
    }
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn launch(args: &[String]) -> Result<ExitCode, String> {
    let root = package_root()?;
    let rivet = env::current_exe().map_err(|error| error.to_string())?;
    let config = root.join("rivet.toml");
    Ok(crate::tunnel::run(&root, &rivet, &config, args))
}

pub fn run(args: Vec<String>) -> ExitCode {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        crate::print_usage(Some("session"));
        return ExitCode::SUCCESS;
    }
    let mut roots = Vec::new();
    let mut passthrough = Vec::new();
    let mut has_command_flags = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--root" {
            match iter.next() {
                Some(root) => roots.push(PathBuf::from(root)),
                None => {
                    eprintln!("--root requires a path");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            if matches!(
                arg.as_str(),
                "--allow-command" | "--allow-subcommand" | "--allow-any-args"
            ) {
                has_command_flags = true;
                let Some(value) = iter.next() else {
                    eprintln!("{arg} requires a value");
                    return ExitCode::FAILURE;
                };
                passthrough.extend([arg, value]);
                continue;
            }
            passthrough.push(arg);
        }
    }
    let cwd = match env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("Cannot read current directory: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let color = io::stdout().is_terminal() && env::var_os("NO_COLOR").is_none();
    let selected = match collect_scope(
        &mut input,
        &mut output,
        &cwd,
        &roots,
        !has_command_flags,
        color,
    ) {
        Ok(scope) => scope,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut scope = scope_args(&selected);
    scope.extend(passthrough);
    match launch(&scope) {
        Ok(status) => status,
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
    fn selection_builds_builtin_and_custom_scope() {
        let cwd = env::current_dir().unwrap();
        let input = Cursor::new("\n3,4,5,7\nhttp\ncurl\nGET,HEAD\ny\nhello\nprintf\n*\nn\n");
        let mut input = input;
        let mut output = Vec::new();
        let scope = collect_scope(&mut input, &mut output, &cwd, &[], true, false).unwrap();
        let args = scope_args(&scope);
        assert!(args.iter().any(|arg| arg == "git=status"));
        assert!(args.iter().any(|arg| arg == "git=rev-parse"));
        assert!(args.iter().any(|arg| arg == "git=ls-files"));
        assert!(args.iter().any(|arg| arg == "cargo=check"));
        assert!(args.iter().any(|arg| arg == "cargo=metadata"));
        assert!(args.iter().any(|arg| arg == "rg=rg"));
        assert!(args.iter().any(|arg| arg == "http=curl"));
        assert!(args.iter().any(|arg| arg == "http=GET"));
        assert!(args.iter().any(|arg| arg == "http=HEAD"));
        assert!(args.iter().any(|arg| arg == "hello=printf"));
        assert!(args.iter().any(|arg| arg == "--allow-any-args"));
        assert!(
            !args
                .iter()
                .any(|arg| arg == "uv=uv" || arg == "mkdir=mkdir" || arg == "make=make")
        );
        let rendered = String::from_utf8(output).unwrap();
        assert!(rendered.contains("7. Add a custom command"));
        assert!(rendered.contains("1,3,7"));
        assert!(rendered.contains("git → git"));
        assert!(rendered.contains("http → curl"));
    }
}
