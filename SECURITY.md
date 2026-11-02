# Security policy

## Supported versions

Vanta is a 0.1 preview. Security reports are accepted against the latest
commit on `main` and the most recent tagged preview release.

## Reporting a vulnerability

Report vulnerabilities privately through GitHub Security Advisories:
<https://github.com/vanta-editor/vanta/security/advisories/new>. Do not open a
public issue for an unpatched vulnerability.

Include the affected version or commit, the platform and terminal, a
description of the impact, and a minimal reproduction if one exists. Please do
not include real secrets, private file contents or personal data in a report.

## Scope

Vanta edits and executes code from untrusted repositories. Areas of interest:

- Terminal escape injection from file contents or process output.
- Swap, backup and state-file permissions or disclosure.
- Path traversal, symlink attacks and unsafe temporary files.
- Panics, hangs or memory exhaustion while parsing files, swap headers or
  command input.
- Command injection through paths, options or external tools.

Vanta performs no telemetry, analytics or automatic update checks. It makes no
network requests unless a future feature is configured to do so; this preview
has no such feature.

Plugins and external language servers do not exist in this preview. When they
ship, plugins will be trusted code and that trust boundary will be documented.
