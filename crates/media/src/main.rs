use clap::{Args, Parser, Subcommand, ValueEnum};
use media::{
    client::{ClientError, HttpClient},
    config::{ClientConfig, ConfigError},
};

#[derive(Debug, Parser)]
#[command(name = "media", version, about = "Personal media orchestration")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Jobs(JobsArgs),
    Queue(QueueArgs),
}

#[derive(Debug, Args)]
struct JobsArgs {
    #[command(subcommand)]
    command: JobsCommand,
}

#[derive(Debug, Subcommand)]
enum JobsCommand {
    Create {
        #[arg(long, value_enum)]
        provider: Provider,
        #[arg(long)]
        result_ref: String,
        #[arg(long)]
        json: bool,
    },
    Get {
        job_id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Args)]
struct QueueArgs {
    #[command(subcommand)]
    command: QueueCommand,
}

#[derive(Debug, Subcommand)]
enum QueueCommand {
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum Provider {
    Rezka,
    Prowlarr,
}

impl From<Provider> for media_contract::ProviderDto {
    fn from(value: Provider) -> Self {
        match value {
            Provider::Rezka => Self::Rezka,
            Provider::Prowlarr => Self::Prowlarr,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum RunError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Client(#[from] ClientError),
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    let output = match cli.command {
        Command::Jobs(JobsArgs {
            command:
                JobsCommand::Create {
                    provider,
                    result_ref,
                    json,
                },
        }) => {
            let _ = json;
            client.create_job(provider.into(), result_ref).await?
        }
        Command::Jobs(JobsArgs {
            command: JobsCommand::Get { job_id, json },
        }) => {
            let _ = json;
            client.get_job(&job_id).await?
        }
        Command::Queue(QueueArgs {
            command: QueueCommand::Status { json },
        }) => {
            let _ = json;
            client.queue_status().await?
        }
    };
    println!("{output}");
    Ok(())
}
