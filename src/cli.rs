//! Command-line interface for the `ruby` binary.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "ruby",
    version,
    about = "Ruby on fusevm — a compiled Ruby runtime (bytecode VM + Cranelift JIT)",
    long_about = None,
)]
pub struct Cli {
    /// Execute a one-liner instead of a file (`ruby -e 'puts 1+1'`).
    #[arg(short = 'e', long = "eval", value_name = "SRC")]
    pub eval: Option<String>,

    /// Start the interactive REPL.
    #[arg(long = "repl")]
    pub repl: bool,

    /// Speak the Language Server Protocol over stdio.
    #[arg(long = "lsp")]
    pub lsp: bool,

    /// Speak the Debug Adapter Protocol over stdio.
    #[arg(long = "dap")]
    pub dap: bool,

    /// Ahead-of-time compile the script's bytecode into the on-disk cache.
    #[arg(long = "build")]
    pub build: bool,

    /// With --build: emit a standalone native executable (no interpreter, no
    /// sources) next to the script instead of warming the cache.
    #[arg(long = "native", requires = "build")]
    pub native: bool,

    /// Print the compiled fusevm bytecode for the script and exit.
    #[arg(long = "dump-bytecode")]
    pub dump_bytecode: bool,

    /// The `.rb` script to run (omit with --repl / --lsp / --dap / -e).
    #[arg(value_name = "FILE")]
    pub file: Option<String>,

    /// Arguments passed through to the Ruby program as `ARGV`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub argv: Vec<String>,
}

/// Parse the process arguments.
pub fn parse() -> Cli {
    Cli::parse()
}
