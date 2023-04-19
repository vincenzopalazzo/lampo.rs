use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[clap(name = "lampod")]
#[clap(about = "An experimental lightning implementation", long_about = None)]
pub struct LampoCliArgs {
    #[clap(short, long, value_parser)]
    pub conf: Option<String>,
    #[clap(short, long, value_parser)]
    pub network: Option<String>,
    #[clap(long, value_parser)]
    pub client: Option<String>,
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
    #[clap(arg_required_else_help = true)]
    GetInfo,
}
