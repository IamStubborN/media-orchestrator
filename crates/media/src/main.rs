use clap::{Args, Parser, Subcommand, ValueEnum};
use media::{
    client::{ClientError, HttpClient},
    composition::{self, ServiceError},
    config::{ClientConfig, ConfigError, DatabaseConfig, RunnerConfig, ServerConfig},
    render,
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
    Release(ReleaseArgs),
    Trending(TrendingArgs),
    Search(SearchArgs),
    #[command(visible_alias = "select")]
    Download(DownloadArgs),
    Rezka(RezkaArgs),
    Migrate,
    Serve,
    Runner,
}

#[derive(Debug, Args)]
struct RezkaArgs {
    #[command(subcommand)]
    command: RezkaCommand,
}

#[derive(Debug, Subcommand)]
enum RezkaCommand {
    Session(RezkaSessionArgs),
    Inspect {
        #[arg(long)]
        locator: String,
        #[arg(long)]
        title_id: u64,
        #[arg(long)]
        translation_id: u64,
        #[arg(long, value_enum)]
        kind: MediaKind,
        #[arg(long)]
        season: Option<u32>,
        #[arg(long)]
        episode: Option<u32>,
        #[arg(long, default_value_t = false)]
        director: bool,
        #[arg(long, default_value_t = false)]
        camrip: bool,
        #[arg(long, default_value_t = false)]
        has_ads: bool,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = false)]
        probe_streams: bool,
    },
}

#[derive(Debug, Args)]
struct RezkaSessionArgs {
    #[command(subcommand)]
    command: RezkaSessionCommand,
}

#[derive(Debug, Subcommand)]
enum RezkaSessionCommand {
    Refresh {
        #[arg(long = "credential-request")]
        credential_request_id: String,
        #[arg(long)]
        json: bool,
    },
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

#[derive(Debug, Args)]
struct ReleaseArgs {
    #[arg(long)]
    title: String,
    #[arg(long)]
    original_title: Option<String>,
    #[arg(long)]
    year: Option<i32>,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct TrendingArgs {
    #[arg(long, value_enum, default_value_t = TrendingCategory::All)]
    category: TrendingCategory,
    #[arg(long, default_value_t = 1, value_parser = parse_positive_page)]
    page: u32,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum TrackingCommand {
    Add {
        #[arg(long, value_enum, default_value = "rezka", hide = true)]
        provider: Provider,
        #[arg(long)]
        title: String,
        #[arg(long)]
        translation: Option<String>,
        #[arg(long = "known-episode", required = true, value_parser = parse_known_episode)]
        known_episodes: Vec<media_contract::EpisodeSnapshotDto>,
        #[arg(long, value_enum)]
        scope: TrackingScope,
        #[arg(long, requires_all = ["translation", "translation_id", "season"])]
        provider_media_ref: Option<String>,
        #[arg(long, requires_all = ["provider_media_ref", "season"])]
        translation_id: Option<u64>,
        #[arg(long, requires_all = ["provider_media_ref", "translation_id"])]
        season: Option<u32>,
        #[arg(long)]
        json: bool,
    },
    EnableDownload {
        tracking_id: String,
        #[arg(long)]
        translation: String,
        #[arg(long)]
        provider_media_ref: String,
        #[arg(long)]
        translation_id: u64,
        #[arg(long)]
        season: u32,
        #[arg(long)]
        json: bool,
    },
    SetBaseline {
        tracking_id: String,
        #[arg(long, value_parser = parse_known_episode)]
        known_through: media_contract::EpisodeSnapshotDto,
        #[arg(long)]
        json: bool,
    },
    CheckNow {
        tracking_id: String,
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
    Retry {
        job_id: String,
        #[arg(long)]
        json: bool,
    },
    Alternatives {
        job_id: String,
        #[arg(long)]
        json: bool,
    },
    MappingAction {
        job_id: String,
        #[arg(long)]
        json: bool,
    },
    ResolveEpisode {
        job_id: String,
        #[arg(long)]
        season: u32,
        #[arg(long)]
        episode: u32,
        #[arg(long)]
        title: Option<String>,
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
enum TrendingCategory {
    All,
    Movie,
    Tv,
}

impl From<TrendingCategory> for media_contract::TrendingCategoryDto {
    fn from(value: TrendingCategory) -> Self {
        match value {
            TrendingCategory::All => Self::All,
            TrendingCategory::Movie => Self::Movie,
            TrendingCategory::Tv => Self::Tv,
        }
    }
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
    #[error(transparent)]
    Diagnostic(#[from] media::diagnostic::DiagnosticError),
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
        Command::Release(args) => run_release(args).await,
        Command::Trending(args) => run_trending(args).await,
        Command::Search(args) => run_search(args).await,
        Command::Download(args) => run_download(args).await,
        Command::Rezka(args) => run_rezka(args).await,
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

async fn run_rezka(args: RezkaArgs) -> Result<(), RunError> {
    match args.command {
        RezkaCommand::Session(args) => {
            let client = HttpClient::new(ClientConfig::load()?)?;
            match args.command {
                RezkaSessionCommand::Refresh {
                    credential_request_id,
                    json,
                } => {
                    let output = client.refresh_rezka_session(credential_request_id).await?;
                    emit(&output, json, |value| {
                        render::job(value, Some("Queued session refresh"))
                    });
                }
            }
        }
        RezkaCommand::Inspect {
            locator,
            title_id,
            translation_id,
            kind,
            season,
            episode,
            director,
            camrip,
            has_ads,
            json,
            probe_streams,
        } => {
            let kind = match (kind, season, episode) {
                (MediaKind::Movie, None, None) => media::diagnostic::InspectionKind::Movie {
                    director,
                    camrip,
                    has_ads,
                },
                (MediaKind::Series, Some(season), Some(episode)) => {
                    media::diagnostic::InspectionKind::Episode { season, episode }
                }
                _ => return Err(media::diagnostic::DiagnosticError::Episode.into()),
            };
            let inspection = media::diagnostic::inspect_playback(
                &RunnerConfig::load()?,
                &locator,
                title_id,
                translation_id,
                kind,
                probe_streams,
            )
            .await?;
            let inspection = inspection.as_json();
            if json {
                println!("{inspection}");
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&inspection)
                        .expect("playback inspection is always serializable")
                );
            }
        }
    }
    Ok(())
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

/// Emit a command result. With `--json` the raw service response is printed
/// verbatim (the machine contract). Otherwise the response is parsed and
/// handed to `render` for a human-readable view, falling back to the raw
/// response if it is not valid JSON.
fn emit(output: &str, json: bool, render: impl FnOnce(&serde_json::Value) -> String) {
    if json {
        println!("{output}");
        return;
    }
    match serde_json::from_str::<serde_json::Value>(output) {
        Ok(value) => println!("{}", render(&value)),
        Err(_) => println!("{output}"),
    }
}

async fn run_tracking(args: TrackingArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    match args.command {
        TrackingCommand::Add {
            provider,
            title,
            translation,
            known_episodes,
            scope,
            provider_media_ref,
            translation_id,
            season,
            json,
        } => {
            let download = match (provider_media_ref, translation_id, season) {
                (Some(provider_media_ref), Some(translation_id), Some(season)) => {
                    Some(media_contract::TrackingDownloadDto {
                        provider_media_ref,
                        translation_id,
                        season,
                    })
                }
                (None, None, None) => None,
                _ => return Err(ClientError::Configuration.into()),
            };
            let translation = translation.unwrap_or_else(|| "release-calendar".to_owned());
            let output = client
                .add_tracking(
                    provider.into(),
                    title,
                    translation,
                    known_episodes,
                    scope.into(),
                    download,
                )
                .await?;
            emit(&output, json, |value| {
                render::tracking(value, Some("Added tracking"))
            });
        }
        TrackingCommand::EnableDownload {
            tracking_id,
            translation,
            provider_media_ref,
            translation_id,
            season,
            json,
        } => {
            let output = client
                .patch_tracking(
                    &tracking_id,
                    media_contract::PatchTrackingRequest {
                        translation,
                        download: media_contract::TrackingDownloadDto {
                            provider_media_ref,
                            translation_id,
                            season,
                        },
                    },
                )
                .await?;
            emit(&output, json, |value| {
                render::tracking(value, Some("Enabled automatic download"))
            });
        }
        TrackingCommand::SetBaseline {
            tracking_id,
            known_through,
            json,
        } => {
            let output = client
                .set_tracking_baseline(&tracking_id, known_through)
                .await?;
            emit(&output, json, |value| {
                render::tracking(value, Some("Updated tracking baseline"))
            });
        }
        TrackingCommand::CheckNow { tracking_id, json } => {
            let output = client.check_tracking_now(&tracking_id).await?;
            emit(&output, json, |value| {
                render::tracking(value, Some("Scheduled tracking check"))
            });
        }
        TrackingCommand::List { json } => {
            let output = client.list_tracking().await?;
            emit(&output, json, render::tracking_list);
        }
        TrackingCommand::Remove { tracking_id, json } => {
            let output = client.remove_tracking(&tracking_id).await?;
            emit(&output, json, |value| {
                render::tracking(value, Some("Removed tracking"))
            });
        }
    }
    Ok(())
}

async fn run_release(args: ReleaseArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    let output = client
        .query_release(media_contract::ReleaseQueryRequest {
            title: args.title,
            original_title: args.original_title,
            year: args.year,
        })
        .await?;
    emit(&output, args.json, render::release);
    Ok(())
}

async fn run_trending(args: TrendingArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    let output = client.trending(args.category.into(), args.page).await?;
    emit(&output, args.json, render::trending);
    Ok(())
}

fn parse_positive_page(value: &str) -> Result<u32, String> {
    match value.parse::<u32>() {
        Ok(page) if page > 0 => Ok(page),
        _ => Err("page must be a positive integer".to_owned()),
    }
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
    emit(&output, args.json, render::search_page);
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
    if episode == 0 {
        return Err("episode must be greater than zero".to_owned());
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
    emit(&output, args.json, |value| {
        render::job(value, Some("Queued download"))
    });
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
    match args.command {
        JobsCommand::Create {
            provider,
            result_ref,
            json,
        } => {
            let output = client.create_job(provider.into(), result_ref).await?;
            emit(&output, json, |value| {
                render::job(value, Some("Created job"))
            });
        }
        JobsCommand::List { json } => {
            let output = client.list_jobs().await?;
            emit(&output, json, render::job_list);
        }
        JobsCommand::Show { job_id, json } => {
            let output = client.get_job(&job_id).await?;
            emit(&output, json, |value| render::job(value, None));
        }
        JobsCommand::Cancel { job_id, json } => {
            let output = client.cancel_job(&job_id).await?;
            emit(&output, json, |value| {
                render::job(value, Some("Cancelled job"))
            });
        }
        JobsCommand::Retry { job_id, json } => {
            let output = client.retry_job(&job_id).await?;
            emit(&output, json, |value| {
                render::job(value, Some("Retried job"))
            });
        }
        JobsCommand::Alternatives { job_id, json } => {
            let output = client
                .alternative_search(&job_id, current_search_scope())
                .await?;
            emit(&output, json, render::alternative_search_page);
        }
        JobsCommand::MappingAction { job_id, json } => {
            let output = client.episode_mapping_action(&job_id).await?;
            emit(&output, json, render::episode_mapping_action);
        }
        JobsCommand::ResolveEpisode {
            job_id,
            season,
            episode,
            title,
            json,
        } => {
            let output = client
                .resolve_episode_mapping(&job_id, season, episode, title)
                .await?;
            emit(&output, json, |value| {
                render::job(value, Some("Resolved episode mapping"))
            });
        }
    }
    Ok(())
}

async fn run_queue(args: QueueArgs) -> Result<(), RunError> {
    let client = HttpClient::new(ClientConfig::load()?)?;
    match args.command {
        QueueCommand::Status { json } => {
            let output = client.queue_status().await?;
            emit(&output, json, render::queue_status);
        }
    }
    Ok(())
}

fn initialize_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .json()
        .try_init();
}
