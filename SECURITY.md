# Security Policy

## Supported versions

Security fixes are applied to the latest released version.

| Version | Supported |
| --- | --- |
| Latest release | Yes |
| Older releases | No |

## Reporting a vulnerability

Do not report a security vulnerability in a public issue.

Use GitHub private vulnerability reporting for this repository when it is available. Include the affected Rivet version or commit, the operating system, the relevant root and command-scope configuration, exact reproduction steps, expected and actual behavior, and the security impact.

Remove runtime API keys, tunnel IDs, access tokens, credentials, and other secrets from logs or examples.

If private vulnerability reporting is unavailable, open a public issue that asks for a private contact path. Do not include vulnerability details or secrets in that issue.

## Security-sensitive areas

Reports are especially useful when they involve command authorization bypass, access outside configured filesystem roots, exposure of runtime credentials or tunnel identifiers, process-control or child-process containment bypass, or validation gaps that allow an unregistered command or disallowed argument to execute.

For the intended trust boundary and documented limitations, see [docs/security.md](docs/security.md).
