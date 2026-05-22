//! Custom Reth binary that installs an ExEx posting native XPL transfer rows to a
//! QuickNode-compatible webhook endpoint.

#![warn(unused_crate_dependencies)]
#![allow(unreachable_pub)]

mod args;
mod exex;
mod extractor;
mod metrics;
mod outbox;
mod payload;
mod replay;
mod webhook;

use crate::{
    args::XplWebhookArgs, exex::xpl_webhook_exex, outbox::Outbox, webhook::WebhookDispatcher,
};
use clap::Parser;
use reth_ethereum::{
    cli::{chainspec::EthereumChainSpecParser, interface::Cli},
    node::EthereumNode,
};

fn main() -> eyre::Result<()> {
    Cli::<EthereumChainSpecParser, XplWebhookArgs>::parse().run(
        async move |builder, webhook_args| {
            let outbox_path = webhook_args
                .outbox_path
                .clone()
                .unwrap_or_else(|| builder.config().datadir().data_dir().join("xpl-webhook"));

            let outbox = Outbox::open(&outbox_path)?;
            let dispatcher = WebhookDispatcher::spawn(webhook_args.clone(), outbox.clone());

            let native_address = webhook_args.native_address;
            let handle = builder
                .node(EthereumNode::default())
                .install_exex("xpl-webhook", {
                    let outbox = outbox.clone();
                    async move |ctx| Ok(xpl_webhook_exex(ctx, outbox, native_address))
                })
                .launch()
                .await?;

            handle.wait_for_node_exit().await?;
            dispatcher.shutdown().await;
            Ok(())
        },
    )
}
