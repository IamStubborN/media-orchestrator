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
    Healthcheck(HealthcheckArgs),
    Jobs(JobsArgs),
    Queue(QueueArgs),
    Tracking(TrackingArgs),
    Search(SearchArgs),
    #[command(visible_alias = "select")]
    Download(DownloadArgs),
    Migrate,
    Serve,
    Runner,
}

#[derive(Debug, Args)]
struct HealthcheckArgs {
    #[arg(long, default_value = "http://127.0.0.1:8080/v1/health")]
    url: reqwest::Url,
}

#[derive(Debug, Args)]
struct TrackingArgs {
    #[command(subcommand)]
    command: TrackingCommand,
}

#[derive(Debug, Subcommand)]
enum TrackingCommand {
    Add {
        #[arg(long, value_enum)]
        provider: Provider,
        #[arg(long)]
        title: String,
        #[arg(long)]
        translation: String,
        #[arg(long = "known-episode", required = true, value_parser = parse_known_episode)]
        known_episodes: Vec<media_contract::EpisodeSnapshotDto>,
        #[arg(long, value_enum)]
        scope: TrackingScope,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Remove {
        tracking_id: String,
        #[arg(long)]
        json: bool,
    },
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

#[derive(Debug, Copy, Clone, ValueEnum)]
enum MediaKind {
    Movie,
    Series,
}

impl From<MediaKind> for media_contract::MediaKindDto {
    fn from(value: MediaKind) -> Self {
        match value {
            MediaKind::Movie => Self::Movie,
            MediaKind::Series => Self::Series,
        }
    }
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum TrackingScope {
    Personal,
    Family,
}

impl From<TrackingScope> for media_contract::TrackingScopeDto {
    fn from(value: TrackingScope) -> Self {
        match value {
            TrackingScope::Personal => Self::Personal,
            TrackingScope::Family => Self::Family,
        }
    }
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
    #[arg(long, value_enum)]
    kind: Option<MediaKind>,
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
    #[error("media service healthcheck failed")]
    Healthcheck,
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
        Command::Healthcheck(args) => run_healthcheck(args).await,
        Command::Jobs(args) => run_jobs(args).await,
        Command::Queue(args) => run_queue(args).await,
        Command::Tracking(args) => run_tracking(args).await,
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

async fn run_tracking(args: TrackingArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    let output = match args.command {
        TrackingCommand::Add {
            provider,
            title,
            translation,
            known_episodes,
            scope,
            json,
        } => {
            let _ = json;
            client
                .add_tracking(
                    provider.into(),
                    title,
                    translation,
                    known_episodes,
                    scope.into(),
                )
                .await?
        }
        TrackingCommand::List { json } => {
            let _ = json;
            client.list_tracking().await?
        }
        TrackingCommand::Remove { tracking_id, json } => {
            let _ = json;
            client.remove_tracking(&tracking_id).await?
        }
    };
    println!("{output}");
    Ok(())
}

async fn run_search(args: SearchArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    let scope = current_search_scope();
    let output = match args.continuation {
        Some(continuation) => client.continue_search(continuation, scope).await?,
        None => {
            client
                .search(media_contract::StartSearchRequest {
                    scope,
                    source: args.source.into(),
                    query: args
                        .query
                        .expect("clap requires query without continuation"),
                    media_kind: args.kind.map(Into::into),
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

fn parse_known_episode(value: &str) -> Result<media_contract::EpisodeSnapshotDto, String> {
    let Some((season, episode)) = value.split_once(':') else {
        return Err("known episode must use SEASON:EPISODE".to_owned());
    };
    let season = season
        .parse::<u32>()
        .map_err(|_| "season must be a positive integer".to_owned())?;
    let episode = episode
        .parse::<u32>()
        .map_err(|_| "episode must be a positive integer".to_owned())?;
    if season == 0 || episode == 0 {
        return Err("season and episode must be greater than zero".to_owned());
    }
    Ok(media_contract::EpisodeSnapshotDto { season, episode })
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
            scope: current_search_scope(),
        })
        .await?;
    let _ = args.json;
    println!("{output}");
    Ok(())
}

fn current_search_scope() -> media_contract::SearchScopeDto {
    let platform = std::env::var("HERMES_SESSION_PLATFORM")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "cli".to_owned());
    let chat_id = std::env::var("HERMES_SESSION_CHAT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "local".to_owned());
    let thread_id = std::env::var("HERMES_SESSION_THREAD_ID")
        .ok()
        .filter(|value| !value.trim().is_empty());
    media_contract::SearchScopeDto {
        platform,
        chat_id,
        thread_id,
    }
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
