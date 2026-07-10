use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "media", version, about = "Personal media orchestration")]
struct Cli {}

fn main() {
    let _ = Cli::parse();
}
