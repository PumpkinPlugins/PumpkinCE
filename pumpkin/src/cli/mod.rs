mod rcon;

use clap::{Parser, Subcommand};

#[derive(Subcommand)]
enum Commands {
    /// Starts a simple RCON client that will automatically connect to your server
    Rcon {
        #[arg(short = 'H', long, help = "Set a different host to connect to")]
        host: Option<String>,

        #[arg(
            trailing_var_arg = true,
            help = "Any command including arguments that should be executed"
        )]
        command: Vec<String>,
    },
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Commands>,
}

pub async fn parse() {
    let args = Args::parse();

    if let Some(Commands::Rcon { host, command }) = args.command {
        rcon::handle_rcon(host, command.join(" ")).await;
        std::process::exit(0);
    }
}
