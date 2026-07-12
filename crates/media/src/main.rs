use clap::{Args, Parser, Subcommand, ValueEnum};
use media::{
    client::{ClientError, HttpClient},
    composition::{self, ServiceError},
    config::{ClientConfig, ConfigError, DatabaseConfig, ServerConfig},
};

#[derive(Debug, Parser)]
#[command(name = "media", version, about = "Personal media orchestration")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Healthcheck(HealthcheckArgs),
    Jobs(JobsArgs),
    Queue(QueueArgs),
    Migrate,
    Serve,
}

#[derive(Debug, Args)]
struct HealthcheckArgs {
    #[arg(long, default_value = "http://127.0.0.1:8080/v1/health")]
    url: reqwest::Url,
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
    List {
        #[arg(long)]
        json: bool,
    },
    #[command(visible_alias = "get")]
    Show {
        job_id: String,
        #[arg(long)]
        json: bool,
    },
    Cancel {
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
    #[error("media service healthcheck failed")]
    Healthcheck,
    #[error(transparent)]
    Service(#[from] ServiceError),
}

#[tokio::main]
async fn main() {
    initialize_tracing();
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), RunError> {
    match cli.command {
        Command::Healthcheck(args) => run_healthcheck(args).await,
        Command::Jobs(args) => run_jobs(args).await,
        Command::Queue(args) => run_queue(args).await,
        Command::Migrate => {
            let config = DatabaseConfig::load()?;
            composition::migrate(&config).await?;
            Ok(())
        }
        Command::Serve => {
            let config = ServerConfig::load()?;
            composition::serve(config).await?;
            Ok(())
        }
    }
}

async fn run_healthcheck(args: HealthcheckArgs) -> Result<(), RunError> {
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|_| RunError::Healthcheck)?
        .get(args.url)
        .send()
        .await
        .map_err(|_| RunError::Healthcheck)?;
    if !response.status().is_success() {
        return Err(RunError::Healthcheck);
    }
    Ok(())
}

async fn run_jobs(args: JobsArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    let output = match args.command {
        JobsCommand::Create {
            provider,
            result_ref,
            json,
        } => {
            let _ = json;
            client.create_job(provider.into(), result_ref).await?
        }
        JobsCommand::List { json } => {
            let _ = json;
            client.list_jobs().await?
        }
        JobsCommand::Show { job_id, json } => {
            let _ = json;
            client.get_job(&job_id).await?
        }
        JobsCommand::Cancel { job_id, json } => {
            let _ = json;
            client.cancel_job(&job_id).await?
        }
    };
    println!("{output}");
    Ok(())
}

async fn run_queue(args: QueueArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    let output = match args.command {
        QueueCommand::Status { json } => {
            let _ = json;
            client.queue_status().await?
        }
    };
    println!("{output}");
    Ok(())
}

fn initialize_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .json()
        .try_init();
}
