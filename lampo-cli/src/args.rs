use clap::{ArgAction, Parser, Subcommand};
use lampo_common::error;
use lampo_common::json;
use std::collections::HashMap;

#[derive(Parser, Debug)]
#[command(
    name = "lampod-cli",
    about = "Lampo Daemon command line",
    version,
    long_about = None
)]
pub struct Cli {
    #[arg(short = 'd', long = "data-dir")]
    pub data_dir: Option<String>,

    #[arg(short = 'n', long, default_value = "testnet")]
    pub network: String,

    #[arg(short, long)]
    pub socket: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    GetInfo,
    ListChannels,
    OpenChannel {
        #[arg(short, long)]
        node_id: String,

        #[arg(long)]
        capacity: u64,

        #[arg(long)]
        push_msat: Option<u64>,

        #[arg(long, action = ArgAction::SetTrue)]
        announce: Option<bool>,
    },

    Connect {
        #[arg(short, long)]
        node_id: String,
        #[arg(short, long)]
        addr: String,
    },

    CreateInvoice {
        #[arg(short, long)]
        amount_msat: Option<u64>,

        #[arg(short, long)]
        description: String,

        #[arg(long, default_value = "3600")]
        expiry: u32,
    },

    PayInvoice {
        #[arg(short, long)]
        invoice: String,
        #[arg(long)]
        amount_msat: Option<u64>,
    },

    Call {
        method: String,
        #[arg(allow_hyphen_values = true)]
        params: Vec<String>,
    },
}

pub struct LampoCliArgs {
    pub socket: String,
    pub method: String,
    pub args: HashMap<String, json::Value>,
}

pub fn parse_args() -> error::Result<LampoCliArgs> {
    let cli = Cli::parse();

    // If data-dir is specified and socket is not specified,
    // we need to get the socket path from it
    // by appending the network name (default: testnet) to the path
    // and adding the socket path (lampod.socket)
    let socket = match cli.socket {
        Some(s) => s,
        None => {
            let data_dir = cli.data_dir.unwrap_or_else(|| {
                #[allow(deprecated)]
                std::env::home_dir()
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_else(|| ".".to_string())
            });
            let data_dir = format!("{data_dir}/.lampo");
            format!("{}/{}/lampod.socket", data_dir, cli.network)
        }
    };

    let (method, args) = match cli.command {
        Commands::GetInfo => ("getinfo".to_string(), HashMap::new()),

        Commands::ListChannels => ("listchannels".to_string(), HashMap::new()),

        Commands::OpenChannel {
            node_id,
            capacity,
            push_msat,
            announce,
        } => {
            let mut args = HashMap::new();
            args.insert("node_id".to_string(), json::json!(node_id));
            args.insert("capacity".to_string(), json::json!(capacity));
            if let Some(push) = push_msat {
                args.insert("push_msat".to_string(), json::json!(push));
            }
            if let Some(announce) = announce {
                args.insert("announce".to_string(), json::json!(announce));
            }
            ("openchannel".to_string(), args)
        }

        Commands::Connect { node_id, addr } => {
            let mut args = HashMap::new();
            args.insert("node_id".to_string(), json::json!(node_id));
            args.insert("addr".to_string(), json::json!(addr));
            ("connect".to_string(), args)
        }

        Commands::CreateInvoice {
            amount_msat,
            description,
            expiry,
        } => {
            let mut args = HashMap::new();
            if let Some(amount) = amount_msat {
                args.insert("amount_msat".to_string(), json::json!(amount));
            }
            args.insert("description".to_string(), json::json!(description));
            args.insert("expiry".to_string(), json::json!(expiry));
            ("createinvoice".to_string(), args)
        }

        Commands::PayInvoice {
            invoice,
            amount_msat,
        } => {
            let mut args = HashMap::new();
            args.insert("invoice".to_string(), json::json!(invoice));
            if let Some(amount) = amount_msat {
                args.insert("amount_msat".to_string(), json::json!(amount));
            }
            ("payinvoice".to_string(), args)
        }

        Commands::Call { method, params } => {
            let mut args = HashMap::new();
            for param in params {
                if let Some((key, value)) = param.split_once('=') {
                    if let Ok(val) = value.parse::<u64>() {
                        args.insert(key.to_string(), json::json!(val));
                    } else if let Ok(val) = value.parse::<bool>() {
                        args.insert(key.to_string(), json::json!(val));
                    } else {
                        args.insert(key.to_string(), json::json!(value));
                    }
                }
            }
            (method, args)
        }
    };

    Ok(LampoCliArgs {
        socket,
        method,
        args,
    })
}
