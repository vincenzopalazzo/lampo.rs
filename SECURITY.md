# Security Policy

The Lampo team takes the security of our software seriously. Lampo is a
Lightning Network implementation that handles private keys and funds, so
we appreciate every effort to responsibly disclose vulnerabilities and we
will make our best effort to address them quickly.

> [!WARNING]
> Lampo is still under heavy development and should be considered
> **experimental software**. Do not use it on mainnet with funds you
> cannot afford to lose.

## Supported Versions

Lampo has no stable releases yet. Security fixes are applied on top of
the `main` branch, so only the latest commit of `main` is supported.

| Version          | Supported          |
| ---------------- | ------------------ |
| `main` (latest)  | :white_check_mark: |
| older commits    | :x:                |

## Reporting a Vulnerability

**Please do NOT report security vulnerabilities through public GitHub
issues, discussions, or pull requests.**

Instead, please report them through one of the following private channels:

1. **GitHub Private Vulnerability Reporting (preferred)** — use the
   ["Report a vulnerability"](https://github.com/vincenzopalazzo/lampo.rs/security/advisories/new)
   button on the [Security tab](https://github.com/vincenzopalazzo/lampo.rs/security)
   of this repository. This opens a private security advisory that is
   visible only to the maintainers.

2. **Email** — send a detailed report to
   [vincenzopalazzodev@gmail.com](mailto:vincenzopalazzodev@gmail.com)
   with the subject line prefixed by `[lampo-security]`.

A good report should include, when possible:

- A description of the vulnerability and its potential impact
  (e.g. loss of funds, key exfiltration, remote crash, privacy leak).
- Steps to reproduce it, a proof of concept, or an exploit script.
- The affected component (e.g. `lampod`, `lampo-common`, `lampo-bdk-wallet`,
  `lampo-httpd`, `lampo-c-ffi`) and the commit you tested against.
- Any suggested mitigation or fix, if you have one.

## What to Expect

- **Acknowledgement:** we will acknowledge your report within 72 hours.
- **Assessment:** we will investigate and keep you informed about the
  progress. We may ask for additional information or guidance.
- **Resolution:** if the vulnerability is confirmed, we will develop a fix
  in private, coordinate a disclosure date with you, and credit you in
  the security advisory (unless you prefer to remain anonymous).
- **Disclosure:** we follow a coordinated disclosure process. The fix and
  the public advisory are released together, giving users time to upgrade
  when the issue is critical. We kindly ask you not to disclose the issue
  publicly before we do.

## Scope Notes

- Bugs in third-party dependencies should be reported upstream, but if a
  dependency's flaw creates a concrete risk for Lampo users (e.g. an LDK
  or BDK issue exploitable through Lampo), feel free to report it to us
  as well so we can coordinate.
- Reports about missing hardening, unsafe defaults, or dangerous
  configurations (e.g. in `lampo.example.conf` or the Docker setup) are
  also welcome through the private channels above.

Thank you for helping keep Lampo and its users safe!
