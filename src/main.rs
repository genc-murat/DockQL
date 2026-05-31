use clap::Parser;
use dol::{Cli, cli};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = cli::run(cli).await {
        eprintln!("\x1b[31mError: {e}\x1b[0m");
        std::process::exit(1);
    }
}
