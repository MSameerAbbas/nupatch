use clap::{Parser, Subcommand};
use nupatch::cli;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(
    name = "nupatch",
    about = "Patch Cursor's CLI and IDE agents to use nushell instead of PowerShell.",
    version = VERSION,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Apply nushell patches to Cursor agents
    Patch {
        /// Patch CLI agent only
        #[arg(long)]
        cli_only: bool,
        /// Patch IDE agent only
        #[arg(long)]
        ide_only: bool,
        /// Preview changes without applying
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Restore all patched files from backups
    Revert,
    /// Show current patch status for CLI and IDE agents
    #[command(alias = "s")]
    Status,
    /// Verify product.json checksums against files on disk
    #[command(alias = "v")]
    Verify,
    /// Recalculate all product.json checksums
    #[command(name = "fix-checksums", alias = "fc")]
    FixChecksums,
}

fn main() -> miette::Result<()> {
    // Render our own help/version through `ui`; defer everything else to clap.
    let args = match Cli::try_parse() {
        Ok(args) => args,
        Err(e)
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            ) =>
        {
            cli::cmd_help(VERSION);
            return Ok(());
        }
        Err(e) if e.kind() == clap::error::ErrorKind::DisplayVersion => {
            cli::cmd_version(VERSION);
            return Ok(());
        }
        Err(e) => e.exit(),
    };

    match args.command {
        Commands::Patch { cli_only, ide_only, dry_run } => cli::cmd_patch(cli_only, ide_only, dry_run)?,
        Commands::Revert => cli::cmd_revert()?,
        Commands::Status => cli::cmd_status()?,
        Commands::Verify => cli::cmd_verify()?,
        Commands::FixChecksums => cli::cmd_fix_checksums()?,
    }
    Ok(())
}
