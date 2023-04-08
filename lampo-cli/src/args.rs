use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[clap(name = "lampo-cli")]
#[clap(about = "An experimental lightning implementation", long_about = None)]
pub struct LampoCliArgs {
    #[clap(short, long, value_parser)]
    pub conf: Option<String>,
    #[clap(short, long, value_parser)]
    pub network: Option<String>,
    #[clap(long, value_parser)]
    pub client: Option<String>,
}
