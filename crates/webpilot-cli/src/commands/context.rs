use clap::{Args, Subcommand};

#[derive(Args)]
pub struct ContextArgs {
    #[command(subcommand)]
    pub command: ContextCommand,
}

#[derive(Subcommand)]
pub enum ContextCommand {
    /// List active contexts
    List,
    /// Close a context or all contexts
    Close {
        /// Context name to close
        name: Option<String>,
        /// Close all contexts
        #[arg(long)]
        all: bool,
    },
}
