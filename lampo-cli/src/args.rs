use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[clap(name = "lampo-cli")]
#[clap(about = "An experimental lightning implementation", long_about = None)]
pub struct LampoCliArgs {
    #[clap(short, long, value_parser)]
    pub socket: String,
    #[clap(subcommand)]
    pub method: LampoCommands,
}

#[derive(Debug, Subcommand)]
pub enum LampoCommands {
    #[clap(arg_required_else_help = true)]
    Connect {
        node_id: String,
        addr: String,
        port: u64,
    },
    #[clap(name = "getinfo")]
    GetInfo,
    Hello,
}
