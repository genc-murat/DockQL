use clap::Parser;
use dol::{Cli, cli, eval, parser};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = cli::run(cli).await {
        let msg = format_colored_error(&e);
        eprintln!("{msg}");
        std::process::exit(1);
    }
}

/// Format an error with ANSI colours if it's a known error type (ParseError, EvalError).
fn format_colored_error(err: &anyhow::Error) -> String {
    // Try to downcast to known types for rich colour formatting
    if let Some(parse_err) = err.downcast_ref::<parser::ParseError>() {
        return parse_err.format_color();
    }
    if let Some(eval_err) = err.downcast_ref::<eval::EvalError>() {
        return eval_err.format_color();
    }
    // For all other errors, use bold red prefix + Display
    format!(
        "{bold}{red}error{reset}: {msg}",
        bold = dol::ANSI_BOLD,
        red = dol::ANSI_FG_RED,
        reset = dol::ANSI_RESET,
        msg = err,
    )
}
