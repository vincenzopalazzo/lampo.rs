//! Regtest chain backends the integration harness can mine against.
//!
//! `LAMPO_CHAIN_BACKEND=core` (default) starts Bitcoin Core through
//! `corepc-node`. `LAMPO_CHAIN_BACKEND=rbitcoin` starts
//! [`rbitcoin`](https://rbitcoin.org) and talks JSON-RPC with the same
//! cookie file the Core path already hands to lampo.
//!
//! CI selects the lane with a matrix env var. Locally, unset means Core.

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use lampo_common::json;

use clightning_testing::prelude::btc::client::bitcoin::Address;
use clightning_testing::prelude::btc::{exe_path, Conf, Node as CoreNode};

/// Which regtest node the harness mines on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainBackendKind {
    Core,
    Rbitcoin,
}

impl ChainBackendKind {
    /// `LAMPO_CHAIN_BACKEND=rbitcoin` selects rbitcoin. Anything else,
    /// including unset, is Core so existing local runs do not change.
    pub fn from_env() -> Self {
        match std::env::var("LAMPO_CHAIN_BACKEND") {
            Ok(value) if value.eq_ignore_ascii_case("rbitcoin") => Self::Rbitcoin,
            Ok(value) if value.eq_ignore_ascii_case("core") || value.is_empty() => Self::Core,
            Ok(value) => {
                log::warn!(
                    target: "lampo-testing",
                    "unknown LAMPO_CHAIN_BACKEND `{value}`, using core"
                );
                Self::Core
            }
            Err(_) => Self::Core,
        }
    }
}

/// Shared miner the harness and `tests` call into.
///
/// Core keeps the `corepc-node` client. rbitcoin has no wallet RPC, so
/// mining goes through `generatetoaddress` on a small blocking client.
/// Both expose cookie user/password because BDK and the bitcoind plugin
/// authenticate with HTTP Basic.
pub struct ChainBackend {
    kind: ChainBackendKind,
    rpc_url: String,
    user: String,
    pass: String,
    core: Option<CoreNode>,
    rbitcoin: Option<RbitcoinNode>,
}

impl ChainBackend {
    pub fn start(conf: &Conf) -> anyhow::Result<Self> {
        match ChainBackendKind::from_env() {
            ChainBackendKind::Core => Self::start_core(conf),
            ChainBackendKind::Rbitcoin => Self::start_rbitcoin(),
        }
    }

    pub fn kind(&self) -> ChainBackendKind {
        self.kind
    }

    pub fn rpc_url(&self) -> String {
        self.rpc_url.clone()
    }

    pub fn cookie(&self) -> (String, String) {
        (self.user.clone(), self.pass.clone())
    }

    /// Mine `blocks` to `address`. Regtest only; the address must be a
    /// regtest address the wallet already knows.
    pub fn generate_to_address(&self, blocks: u64, address: &str) -> anyhow::Result<()> {
        match self.kind {
            ChainBackendKind::Core => {
                let core = self
                    .core
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("core backend missing"))?;
                let address: Address<lampo_common::bitcoin::address::NetworkUnchecked> = address
                    .parse()
                    .map_err(|err| anyhow::anyhow!("regtest address `{address}`: {err}"))?;
                let address = address.assume_checked();
                core.client
                    .generate_to_address(blocks as usize, &address)
                    .map_err(|err| anyhow::anyhow!("core generatetoaddress: {err}"))?;
                Ok(())
            }
            ChainBackendKind::Rbitcoin => {
                let node = self
                    .rbitcoin
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("rbitcoin backend missing"))?;
                node.generate_to_address(blocks, address)
            }
        }
    }

    /// Tip height from `getblockchaininfo`.
    ///
    /// Core uses the typed client. rbitcoin is parsed from the raw JSON
    /// `blocks` field so a missing Core-only key cannot fail the fund wait.
    /// `None` means the call itself failed.
    pub fn tip_height(&self) -> Option<i64> {
        match self.kind {
            ChainBackendKind::Core => {
                let core = self.core.as_ref()?;
                let info = core.client.get_blockchain_info().ok()?;
                Some(info.blocks as i64)
            }
            ChainBackendKind::Rbitcoin => {
                let node = self.rbitcoin.as_ref()?;
                let value = node.rpc("getblockchaininfo", "[]").ok()?;
                value.get("blocks")?.as_i64()
            }
        }
    }

    /// Core node, when this backend is Core. Tests that share one miner
    /// clone this. rbitcoin tests share [`ChainBackend`] itself.
    pub fn core(&self) -> Option<&CoreNode> {
        self.core.as_ref()
    }

    fn start_core(conf: &Conf) -> anyhow::Result<Self> {
        let exec = exe_path().map_err(|err| anyhow::anyhow!("corepc-node exec path: {err}"))?;
        let core = CoreNode::with_conf(exec, conf)?;
        let url = core.rpc_url();
        let cookie = core
            .params
            .get_cookie_values()
            .map_err(|err| anyhow::anyhow!("core cookie: {err}"))?
            .ok_or_else(|| anyhow::anyhow!("core cookie file was empty"))?;
        Ok(Self {
            kind: ChainBackendKind::Core,
            rpc_url: url,
            user: cookie.user,
            pass: cookie.password,
            core: Some(core),
            rbitcoin: None,
        })
    }

    fn start_rbitcoin() -> anyhow::Result<Self> {
        let node = RbitcoinNode::start()?;
        Ok(Self {
            kind: ChainBackendKind::Rbitcoin,
            rpc_url: node.rpc_url.clone(),
            user: node.user.clone(),
            pass: node.pass.clone(),
            core: None,
            rbitcoin: Some(node),
        })
    }
}

/// rbitcoin process plus the cookie it was started with.
///
/// The binary is `LAMPO_RBITCOIN` or `rbitcoin-node` on `PATH`. CI
/// downloads a release and exports the env var; a missing binary is a
/// setup error, not a silent fallback to Core.
struct RbitcoinNode {
    process: Child,
    datadir: tempfile::TempDir,
    rpc_url: String,
    user: String,
    pass: String,
}

impl RbitcoinNode {
    fn start() -> anyhow::Result<Self> {
        let binary = std::env::var("LAMPO_RBITCOIN").unwrap_or_else(|_| "rbitcoin-node".to_owned());
        if !Path::new(&binary).exists() && which_on_path(&binary).is_none() {
            anyhow::bail!(
                "rbitcoin binary `{binary}` not found. Set LAMPO_RBITCOIN or install rbitcoin-node"
            );
        }
        let datadir = tempfile::tempdir()?;
        let user = "__cookie__".to_owned();
        let pass = random_hex(32);
        let cookie = datadir.path().join("rpc.cookie");
        // rbitcoin refuses a trailing newline in the cookie file.
        fs::write(&cookie, format!("{user}:{pass}"))?;

        let port = free_port()?;
        let rpc_url = format!("http://127.0.0.1:{port}");
        let mut command = Command::new(&binary);
        command
            .arg("--network")
            .arg("regtest")
            .arg("--datadir")
            .arg(datadir.path())
            .arg("--no-listen")
            .arg("--no-seeds")
            .arg("--rpc-listen")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--rpc-cookie-file")
            .arg(&cookie)
            .arg("--log-level")
            .arg("warn")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let process = command
            .spawn()
            .map_err(|err| anyhow::anyhow!("spawn `{binary}`: {err}"))?;

        let node = Self {
            process,
            datadir,
            rpc_url,
            user,
            pass,
        };
        node.wait_for_rpc()?;
        Ok(node)
    }

    fn rpc(&self, method: &str, params: &str) -> anyhow::Result<json::Value> {
        let body =
            format!(r#"{{"jsonrpc":"1.0","id":"lampo","method":"{method}","params":{params}}}"#);
        let output = Command::new("curl")
            .args([
                "-fsS",
                "--max-time",
                "30",
                "-u",
                &format!("{}:{}", self.user, self.pass),
                &self.rpc_url,
                "-H",
                "content-type: application/json",
                "-d",
                &body,
            ])
            .output()
            .map_err(|err| anyhow::anyhow!("curl {method}: {err}"))?;
        if !output.status.success() {
            anyhow::bail!(
                "{method} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let parsed: json::Value = json::from_slice(&output.stdout)
            .map_err(|err| anyhow::anyhow!("{method} was not json: {err}"))?;
        if let Some(err) = parsed.get("error").filter(|err| !err.is_null()) {
            anyhow::bail!("{method}: {err}");
        }
        Ok(parsed.get("result").cloned().unwrap_or(json::Value::Null))
    }

    fn generate_to_address(&self, blocks: u64, address: &str) -> anyhow::Result<()> {
        let body = format!(
            r#"{{"jsonrpc":"1.0","id":"lampo","method":"generatetoaddress","params":[{blocks},"{address}"]}}"#
        );
        let output = Command::new("curl")
            .args([
                "-fsS",
                "--max-time",
                "120",
                "-u",
                &format!("{}:{}", self.user, self.pass),
                &self.rpc_url,
                "-H",
                "content-type: application/json",
                "-d",
                &body,
            ])
            .output()?;
        if !output.status.success() {
            anyhow::bail!(
                "rbitcoin generatetoaddress: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let parsed: json::Value = json::from_slice(&output.stdout)?;
        if let Some(err) = parsed.get("error").filter(|err| !err.is_null()) {
            anyhow::bail!("rbitcoin generatetoaddress: {err}");
        }
        Ok(())
    }

    fn wait_for_rpc(&self) -> anyhow::Result<()> {
        for _ in 0..50 {
            let ok = Command::new("curl")
                .args([
                    "-fsS",
                    "--max-time",
                    "2",
                    "-u",
                    &format!("{}:{}", self.user, self.pass),
                    &self.rpc_url,
                    "-H",
                    "content-type: application/json",
                    "-d",
                    r#"{"jsonrpc":"1.0","id":"1","method":"getblockcount","params":[]}"#,
                ])
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            if ok {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        anyhow::bail!("rbitcoin RPC at {} did not answer within 5s", self.rpc_url)
    }
}

impl Drop for RbitcoinNode {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
        // TempDir removes the 1 GB archive preallocation on drop.
        let _ = &self.datadir;
    }
}

fn free_port() -> anyhow::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn random_hex(bytes: usize) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Not a secret. The cookie only authenticates a loopback regtest node
    // that dies with the test. Mixing the clock with the pid is enough to
    // keep two parallel tests from sharing a password.
    let ticks = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut state = ticks ^ ((std::process::id() as u128) << 64);
    let mut out = String::with_capacity(bytes * 2);
    while out.len() < bytes * 2 {
        state = state
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0x6A09_E667_F3BC_C909);
        out.push_str(&format!("{state:032x}"));
    }
    out.truncate(bytes * 2);
    out
}

fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(name);
        candidate.is_file().then_some(candidate)
    })
}
