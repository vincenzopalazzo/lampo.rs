use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[clap(name = "coffee")]
#[clap(about = "A plugin manager for core lightning", long_about = None)]
pub struct LampoCliArgs {
    #[clap(short, long, value_parser)]
    pub conf: Option<String>,
    #[clap(short, long, value_parser)]
    pub network: Option<String>,
}
