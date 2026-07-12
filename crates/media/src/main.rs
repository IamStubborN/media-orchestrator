use clap::{Args, Parser, Subcommand, ValueEnum};
use media::{
    client::{ClientError, HttpClient},
    composition::{self, ServiceError},
    config::{ClientConfig, ConfigError, DatabaseConfig, RunnerConfig, ServerConfig},
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
    Search(SearchArgs),
    #[command(visible_alias = "select")]
    Download(DownloadArgs),
    Migrate,
    Serve,
    Runner,
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
    #[command(visible_aliases = ["get", "status"])]
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

#[derive(Debug, Args)]
struct SearchArgs {
    #[arg(value_enum)]
    source: Provider,
    #[arg(
        required_unless_present = "continuation",
        conflicts_with = "continuation"
    )]
    query: Option<String>,
    #[arg(long = "continue", conflicts_with = "query")]
    continuation: Option<String>,
    #[arg(long)]
    season: Option<u16>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct DownloadArgs {
    #[arg(long)]
    session: String,
    #[arg(long)]
    result: String,
    #[arg(long)]
    translation_id: Option<u64>,
    #[arg(long)]
    season: Option<u32>,
    #[arg(long)]
    episode: Option<u32>,
    #[arg(long)]
    json: bool,
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
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Runner(#[from] composition::RunnerError),
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
        Command::Jobs(args) => run_jobs(args).await,
        Command::Queue(args) => run_queue(args).await,
        Command::Search(args) => run_search(args).await,
        Command::Download(args) => run_download(args).await,
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
        Command::Runner => {
            composition::run_runner(RunnerConfig::load()?).await?;
            Ok(())
        }
    }
}

async fn run_search(args: SearchArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    let output = match args.continuation {
        Some(continuation) => client.continue_search(continuation).await?,
        None => {
            client
                .search(media_contract::StartSearchRequest {
                    source: args.source.into(),
                    query: args
                        .query
                        .expect("clap requires query without continuation"),
                    season: args.season,
                    preferred_qualities: Vec::new(),
                    preferred_languages: Vec::new(),
                    preferred_codecs: Vec::new(),
                    preferred_release_groups: Vec::new(),
                })
                .await?
        }
    };
    let _ = args.json;
    println!("{output}");
    Ok(())
}

async fn run_download(args: DownloadArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    let output = client
        .select(media_contract::SelectResultRequest {
            session_id: args.session,
            result_id: args.result,
            translation_id: args.translation_id,
            season: args.season,
            episode: args.episode,
        })
        .await?;
    let _ = args.json;
    println!("{output}");
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
