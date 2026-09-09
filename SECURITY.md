# Security Policy

## Supported versions

Flux is pre-release. Until 0.1.0 ships there is no supported version and no
backported fixes — security work lands on `main`.

| Version | Supported |
|---|---|
| `main` (pre-0.1) | ✅ |

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Report privately through GitHub's private vulnerability reporting: go to the
[Security tab](https://github.com/ckir/flux/security/advisories) of this repository
and choose **Report a vulnerability**. That opens a private advisory visible only
to the maintainers.

Please include:

- A description of the issue and the impact you believe it has
- Steps to reproduce, ideally a minimal one
- The OS, filesystem type (source *and* destination), and Flux version
- Any relevant `--json` output or logs, with paths redacted if sensitive

You can expect an acknowledgement within a few days. Please give us a reasonable
window to ship a fix before disclosing publicly.

## What counts as a vulnerability in Flux

Flux is an offline filesystem transfer engine — it has no network surface, no
server and no authentication. The threat model is therefore mostly about what a
copy can be tricked into doing to a filesystem. Reports we especially want:

- **Escaping the destination.** A source tree that causes writes outside the
  destination root — via symlinks, hardlinks, `..` components, junctions,
  reparse points, or path normalization differences between platforms.
- **Data destruction.** Any path where Flux overwrites or truncates data it was
  not asked to touch, or where an interrupted or resumed operation leaves the
  destination in a state the user did not request.
- **Silent corruption.** A copy that reports success while the destination
  differs from the source, or verification that passes when it should not.
- **Time-of-check/time-of-use races.** Source mutation, identity reuse or inode
  recycling that Flux fails to detect and that leads to the wrong bytes landing
  at the wrong path.
- **Operation-state abuse.** A crafted or hostile operation workspace, manifest,
  lock or topology store that causes Flux to misbehave when it resumes or cleans
  up.
- **Privilege or metadata escalation.** Permissions, ACLs or ownership at the
  destination that are more permissive than intended.

Crashes and panics on malformed input are bugs — please file them as normal
issues unless they lead to one of the outcomes above.

## Scope

Out of scope: vulnerabilities in dependencies (report those upstream, though
please tell us so we can pin or drop them), and issues that require an attacker
who already has write access to the destination or to Flux's own operation
workspace.
