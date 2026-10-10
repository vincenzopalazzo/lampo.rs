# Security Policy

The Lampo team takes the security of our software seriously. Lampo is a
Lightning Network implementation that handles private keys and funds, so
we appreciate every effort to responsibly disclose vulnerabilities and we
will make our best effort to address them quickly.

> [!WARNING]
> Lampo is still under heavy development and should be considered
> **experimental software**. Do not use it on mainnet with funds you
> cannot afford to lose.

## Peer backup

Lampo implements BOLT 1 peer storage (`option_provide_storage`, bits 42/43).
Those bits mean we will store a peer's opaque blob. Our own outbound backup is
a separate path and is compiled in only with `--cfg peer_storage`.

- A peer's blob is stored inside `ChannelManager` state. BOLT 1 allows a
  message of 65531 bytes; LDK persists at most 1 KiB per funded peer and
  rejects anything larger with a warning.
- Our own backup is an encrypted channel-monitor snapshot, sent on each new
  best block to peers we have a funded channel with. LDK packs that snapshot
  up to the BOLT message limit (65531 bytes). A serialized monitor is larger
  than 1 KiB, so another Lampo or LDK node rejects it with a warning and does
  not store it. Lampo-to-LDK outbound backups are not durable today. It is not
  a substitute for the on-disk channel monitor.
- If a `peer_storage_retrieval` blob is ahead of local channel state, LDK
  panics while handling the message. The panic text mentions a `FundRecoverer`
  helper; that helper is not implemented (it is a TODO in LDK). Do not treat
  peer storage as unattended disaster recovery.

`.cargo/config.toml` sets the cfg for normal builds. `RUSTFLAGS` and
`CARGO_ENCODED_RUSTFLAGS` replace that file, so those builds must append
`--cfg peer_storage`. `lampod`'s `build.rs` refuses to compile without it.
rustdoc does not read `.cargo/config.toml`; the check is not in the crate, so
doctests are not a false failure.

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
  `lampo-httpd`) and the commit you tested against.
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
