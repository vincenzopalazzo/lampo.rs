//! Channel configuration the LSP client needs from the node.

use lampo_common::bitcoin::secp256k1::PublicKey;
use lampo_common::ldk::ln::types::ChannelId;
use lampo_common::ldk::util::config::ChannelConfigUpdate;
use lampo_common::types::LampoChannel;

/// Let every channel with `counterparty` deliver HTLCs whose amount is
/// below what the onion promised, so the LSP can take its funding fee from
/// them. The claim path still checks that fee against a recorded purchase
/// before releasing a preimage. Returns how many channels with
/// `counterparty` exist.
pub fn accept_underpaying_htlcs_from(
    channel_manager: &LampoChannel,
    counterparty: &PublicKey,
) -> usize {
    let update = ChannelConfigUpdate {
        accept_underpaying_htlcs: Some(true),
        ..Default::default()
    };
    let channels: Vec<ChannelId> = channel_manager
        .list_channels()
        .into_iter()
        .filter(|channel| channel.counterparty.node_id == *counterparty)
        .map(|channel| channel.channel_id)
        .collect();
    for channel_id in &channels {
        match channel_manager.update_partial_channel_config(counterparty, &[*channel_id], &update) {
            Ok(()) => log::info!(
                target: "phoenix-lsp",
                "accepting underpaying HTLCs on `{channel_id}` from `{counterparty}`"
            ),
            Err(err) => log::warn!(
                target: "phoenix-lsp",
                "cannot update the config of `{channel_id}` with `{counterparty}`: {err:?}"
            ),
        }
    }
    channels.len()
}
