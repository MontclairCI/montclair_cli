use clap::Args;
use clap::Subcommand;
use http::Request;
use http::StatusCode;
use http_body_util::BodyExt;
use muzanci_config::config::DebugClientConfig;
use muzanci_config::config::DebugSessionId;
use muzanci_config::config::ImageConfig;
use muzanci_config::config::ServerId;
use muzanci_transport::channel::ChannelReceiver;
use muzanci_transport::channel::ChannelSender;
use muzanci_transport::channel::ChannelType;
use muzanci_transport::http::CliCreateDebugSessionRequest;
use muzanci_transport::http::CliCreateDebugSessionResponse;
use muzanci_transport::http::CliGetDebugClientConfigResponse;
use muzanci_transport::http::CliGetUserResponse;
use muzanci_transport::message::DebugClientMessage;
use muzanci_transport::message::Message;
use muzanci_transport::message::UserId;
use sha2::Sha256;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::io::BufReader;
use tokio_util::sync::CancellationToken;
use url::Url;

use muzanci_config::collector::Collector;
use muzanci_config::config::JobConfig;
use muzanci_git::GitClient;
use muzanci_git::GitRemote;
use muzanci_observability::TError;
use muzanci_transport::channel::FnChannelAcceptor;
use muzanci_transport::http::DebugClientResponse;
use muzanci_transport::http::MUZANCI_TRANSPORT_V1;
use muzanci_transport::message::AccountId;
use muzanci_transport::mux::Mux;
use muzanci_transport::mux::MuxHandle;

use crate::cli::parse::parse_debug_session_id;
use crate::cli::parse::parse_env;
use crate::cli::parse::parse_key_val;
use crate::cli::user_config::UserConfig;
use crate::ssh_tunnel::DebugClientSshTunnel;
use crate::stdin::StdinStream;

#[derive(Subcommand, Clone)]
pub enum DebugSubcommand {
    #[command(name = "interactive", about = "Start an interactive debug session")]
    Interactive {
        #[command(flatten)]
        args: DebugInteractiveArgs,
    },

    #[command(name = "create", about = "Create a new debug session")]
    Create {
        #[command(flatten)]
        args: DebugCreateArgs,
    },

    #[command(name = "exec", about = "Execute a command in the debug session")]
    Exec {
        #[command(flatten)]
        args: DebugExecArgs,
    },

    #[command(name = "step", about = "Execute a job step in the debug session")]
    ExecStep {
        #[command(flatten)]
        args: DebugExecStepArgs,
    },
}

#[derive(Args, Clone, Debug)]
pub struct DebugInteractiveArgs {
    #[arg(
        long = "debug-session-id",
        value_parser = parse_debug_session_id,
        help = "The ID of the debug session. If not provided, a new debug session will be created."
    )]
    debug_session_id_opt: Option<DebugSessionId>,

    #[arg(
        long = "input",
        value_name = "FILE",
        default_value = "muzan.py",
        help = "The path to the input CI config file."
    )]
    ci_config_path: PathBuf,

    #[arg(
        long = "env",
        value_name = "KEY=VALUE",
        value_parser = parse_key_val,
        action = clap::ArgAction::Append,
        help = "Environment variables to set for the CI config parsing.",
    )]
    ci_env: Vec<(String, String)>,

    #[arg(
        long = "job",
        name = "job",
        value_name = "JOB",
        help = "The name of the CI job to debug."
    )]
    ci_job: String,

    #[arg(
        long = "git-branch",
        value_name = "GIT_REPO_PATH",
        default_value = ".",
        help = "The path to the git repository."
    )]
    ci_git_repo_path: PathBuf,

    #[arg(
        long = "git-remote",
        value_name = "GIT_REMOTE",
        default_value = "origin",
        help = "The name of the git remote."
    )]
    ci_git_remote_name: String,

    #[arg(
        long = "server_hostname",
        value_name = "SERVER_HOSTNAME",
        default_value = "papaya-repeal-curvature.ngrok-free.dev",
        help = "The hostname of the server to connect to."
    )]
    server_hostname: String,
}

#[derive(Args, Clone, Debug)]
pub struct DebugCreateArgs {
    #[arg(
        long = "input",
        value_name = "FILE",
        default_value = "muzan.py",
        help = "The path to the input CI config file."
    )]
    ci_config_path: PathBuf,

    #[arg(
        long = "env",
        value_name = "KEY=VALUE",
        value_parser = parse_key_val,
        action = clap::ArgAction::Append,
        help = "Environment variables to set for the CI config parsing."
    )]
    ci_env: Vec<(String, String)>,

    #[arg(
        long = "git-branch",
        value_name = "GIT_REPO_PATH",
        default_value = ".",
        help = "The path to the git repository."
    )]
    ci_git_repo_path: PathBuf,

    #[arg(
        long = "git-remote",
        value_name = "GIT_REMOTE",
        default_value = "origin",
        help = "The name of the git remote."
    )]
    ci_git_remote_name: String,

    #[arg(
        long = "job",
        value_name = "JOB",
        help = "The name of the CI job to debug."
    )]
    ci_job: String,

    #[arg(
        long,
        name = "server_hostname",
        value_name = "SERVER_HOSTNAME",
        default_value = "papaya-repeal-curvature.ngrok-free.dev"
    )]
    server_hostname: String,
}

#[derive(Args, Clone, Debug)]
pub struct DebugExecArgs {
    #[arg(
        long = "debug-session-id",
        value_parser = parse_debug_session_id,
        help = "The ID of the debug session."
    )]
    debug_session_id: DebugSessionId,

    #[arg(long = "command", help = "The shell command to execute")]
    command: String,

    #[arg(
        long = "server_hostname",
        value_name = "SERVER_HOSTNAME",
        default_value = "papaya-repeal-curvature.ngrok-free.dev"
    )]
    server_hostname: String,
}

#[derive(Args, Clone, Debug)]
pub struct DebugExecStepArgs {
    #[arg(
        long = "debug-session-id",
        value_parser = parse_debug_session_id,
        help = "The ID of the debug session."
    )]
    debug_session_id: DebugSessionId,

    #[arg(
        long = "input",
        value_name = "FILE",
        default_value = "muzan.py",
        help = "The path to the input CI config file."
    )]
    ci_config_path: PathBuf,

    #[arg(
        long = "env",
        value_name = "KEY=VALUE",
        value_parser = parse_key_val,
        action = clap::ArgAction::Append,
        help = "Environment variables to set for the CI config parsing."
    )]
    ci_env: Vec<(String, String)>,

    #[arg(long = "job", value_name = "JOB", help = "The name of a CI job.")]
    ci_job: String,

    #[arg(
        long = "step",
        value_name = "STEP",
        help = "The name of the step in the job to execute."
    )]
    ci_step: String,

    #[arg(
        long,
        name = "server_hostname",
        value_name = "SERVER_HOSTNAME",
        default_value = "papaya-repeal-curvature.ngrok-free.dev"
    )]
    server_hostname: String,
}

#[tracing::instrument(skip_all)]
pub async fn run_debug_interactive(
    args: DebugInteractiveArgs,
    mut user_config: UserConfig,
    client_shutdown: CancellationToken,
    signal_receiver_enabled: Arc<AtomicBool>,
) -> Result<(), TError> {
    let DebugInteractiveArgs {
        debug_session_id_opt,
        ci_config_path,
        ci_env,
        ci_job,
        ci_git_repo_path,
        ci_git_remote_name,
        server_hostname,
    } = args;

    let job = {
        let env = parse_env(ci_env)?;

        let collector = Collector::new(&env);

        collector.evaluate(&ci_config_path).map_err(|e| {
            TError::new(&format!(
                "failed to evaluate {}:\n{}",
                ci_config_path.display(),
                e
            ))
        })?;

        collector
            .jobs()
            .into_iter()
            .find(|j| j.name == ci_job)
            .ok_or_else(|| TError::new(&format!("job not found: {ci_job}")))?
    };

    println!("Found job: {}", job.name);
    println!("Job steps:");

    for step in &job.steps {
        println!("  - {}", step.name);
    }

    let secret = user_config.require_personal_access_token()?;

    let user_id = fetch_user_id(&server_hostname, &secret)
        .await?
        .ok_or_else(|| TError::new("Unable to find user for your personal access token"))?;

    let account_id = user_config
        .require_account_id(&server_hostname, &secret)
        .await?;

    // We prefer to parse git remote before creating debug session so we can fail fast if the local
    // git repo is not set up correctly.
    let git_remote =
        GitClient::try_default()?.get_remote(&ci_git_repo_path, &ci_git_remote_name)?;

    let capacity = 1;

    let debug_session_id =
        create_debug_session(&server_hostname, user_id, &secret, account_id, capacity).await?;

    let debug_client_config = get_debug_client_config(
        &server_hostname,
        user_id,
        &secret,
        account_id,
        debug_session_id,
    )
    .await?
    .ok_or_else(|| TError::new("Unabled to find debug session"))?;

    let mut debug_client = DebugClient::connect(
        client_shutdown,
        &server_hostname,
        user_id,
        &secret,
        account_id,
        debug_client_config,
    )
    .await?;

    if debug_session_id_opt.is_none() {
        debug_client
            .prepare_sandbox(job.image.clone(), git_remote)
            .await?;
    }

    debug_client
        .start_interactive_session(signal_receiver_enabled, job)
        .await?;

    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn run_debug_create(
    args: DebugCreateArgs,
    mut user_config: UserConfig,
    client_shutdown: CancellationToken,
) -> Result<(), TError> {
    let DebugCreateArgs {
        ci_config_path,
        ci_job,
        ci_env,
        ci_git_repo_path,
        ci_git_remote_name,
        server_hostname,
    } = args;

    let image = {
        let env = parse_env(ci_env)?;

        let collector = Collector::new(&env);

        collector.evaluate(&ci_config_path).map_err(|e| {
            TError::new(&format!(
                "failed to evaluate {}:\n{}",
                ci_config_path.display(),
                e
            ))
        })?;

        let job = collector
            .jobs()
            .into_iter()
            .find(|j| j.name == ci_job || j.job_id.to_string() == ci_job)
            .ok_or_else(|| TError::new(&format!("job not found: {ci_job}")))?;

        job.image
    };

    let secret = user_config.require_personal_access_token()?;

    let user_id = fetch_user_id(&server_hostname, &secret)
        .await?
        .ok_or_else(|| TError::new("Unable to find user for your personal access token"))?;

    let account_id = user_config
        .require_account_id(&server_hostname, &secret)
        .await?;

    let git_remote =
        GitClient::try_default()?.get_remote(&ci_git_repo_path, &ci_git_remote_name)?;

    let capacity = 1;

    let debug_session_id =
        create_debug_session(&server_hostname, user_id, &secret, account_id, capacity).await?;

    let debug_client_config = get_debug_client_config(
        &server_hostname,
        user_id,
        &secret,
        account_id,
        debug_session_id,
    )
    .await?
    .ok_or_else(|| TError::new("Unabled to find debug session"))?;

    let mut debug_client = DebugClient::connect(
        client_shutdown,
        &server_hostname,
        user_id,
        &secret,
        account_id,
        debug_client_config,
    )
    .await?;

    debug_client.prepare_sandbox(image, git_remote).await?;

    // TODO: Print debug session ID and metadata. Provide instructions to continue debugging.

    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn run_debug_exec(
    args: DebugExecArgs,
    mut user_config: UserConfig,
    client_shutdown: CancellationToken,
    signal_receiver_enabled: Arc<AtomicBool>,
) -> Result<(), TError> {
    let DebugExecArgs {
        debug_session_id,
        command,
        server_hostname,
    } = args;

    let secret = user_config.require_personal_access_token()?;

    let user_id = fetch_user_id(&server_hostname, &secret)
        .await?
        .ok_or_else(|| TError::new("Unable to find user for your personal access token"))?;

    let account_id = user_config
        .require_account_id(&server_hostname, &secret)
        .await?;

    let debug_client_config = get_debug_client_config(
        &server_hostname,
        user_id,
        &secret,
        account_id,
        debug_session_id,
    )
    .await?
    .ok_or_else(|| TError::new("Unabled to find debug session"))?;

    let mut debug_client = DebugClient::connect(
        client_shutdown,
        &server_hostname,
        user_id,
        &secret,
        account_id,
        debug_client_config,
    )
    .await?;

    debug_client.exec(signal_receiver_enabled, &command).await?;

    Ok(())
}

#[tracing::instrument(skip_all)]
pub async fn run_debug_exec_step(
    args: DebugExecStepArgs,
    mut user_config: UserConfig,
    client_shutdown: CancellationToken,
    signal_receiver_enabled: Arc<AtomicBool>,
) -> Result<(), TError> {
    let DebugExecStepArgs {
        debug_session_id,
        ci_config_path,
        ci_job,
        ci_step,
        ci_env,
        server_hostname,
    } = args;

    let step = {
        let env = parse_env(ci_env)?;

        let collector = Collector::new(&env);

        collector.evaluate(&ci_config_path).map_err(|e| {
            TError::new(&format!(
                "failed to evaluate CI config {}:\n{}",
                ci_config_path.display(),
                e
            ))
        })?;

        let job = collector
            .jobs()
            .into_iter()
            .find(|j| j.name == ci_job || j.job_id.to_string() == ci_job)
            .ok_or_else(|| TError::new(&format!("Job not found: {ci_job}")))?;

        job.steps
            .into_iter()
            .find(|s| s.name == ci_step)
            .ok_or_else(|| TError::new(&format!("Step not found: {ci_step}")))?
    };

    let secret = user_config.require_personal_access_token()?;

    let user_id = fetch_user_id(&server_hostname, &secret)
        .await?
        .ok_or_else(|| TError::new("Unable to find user for your personal access token"))?;

    let secret = user_config.require_personal_access_token()?;

    let account_id = user_config
        .require_account_id(&server_hostname, &secret)
        .await?;

    let debug_client_config = get_debug_client_config(
        &server_hostname,
        user_id,
        &secret,
        account_id,
        debug_session_id,
    )
    .await?
    .ok_or_else(|| TError::new("Unabled to find debug session"))?;

    let mut debug_client = DebugClient::connect(
        client_shutdown,
        &server_hostname,
        user_id,
        &secret,
        account_id,
        debug_client_config,
    )
    .await?;

    debug_client
        .exec(signal_receiver_enabled, &step.command)
        .await?;

    Ok(())
}

#[tracing::instrument(skip_all)]
async fn fetch_user_id(server_hostname: &str, secret: &str) -> Result<Option<UserId>, TError> {
    let url = Url::parse(&format!("https://{server_hostname}/cli_user"))?;

    let response = reqwest::Client::new()
        .get(url)
        .header("x-muzanci-personal-access-token-secret", secret)
        .header("accept", "application/json")
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await?;
        return Err(TError::new(&format!(
            "failed to fetch user: HTTP [{status}] - [{body}]"
        )));
    }

    let json = response.json::<CliGetUserResponse>().await?;

    Ok(json.user_id_opt)
}

#[tracing::instrument(skip_all)]
async fn create_debug_session(
    server_hostname: &str,
    user_id: UserId,
    secret: &str,
    account_id: AccountId,
    capacity: u64,
) -> Result<DebugSessionId, TError> {
    let url = Url::parse(&format!(
        "https://{server_hostname}/cli_create_debug_session"
    ))?;

    let response = reqwest::Client::new()
        .post(url)
        .header("x-muzanci-user-id", user_id.to_string())
        .header("x-muzanci-personal-access-token-secret", secret)
        .header("x-muzanci-account-id", account_id.to_string())
        .header("accept", "application/json")
        .json(&CliCreateDebugSessionRequest { capacity })
        .send()
        .await?;

    if !response.status().is_success() {
        let body = response.text().await?;
        return Err(TError::new(&format!(
            "failed to create debug session: {body}"
        )));
    }

    let json = response.json::<CliCreateDebugSessionResponse>().await?;

    Ok(json.debug_session_id)
}

#[tracing::instrument(skip_all)]
async fn get_debug_client_config(
    server_hostname: &str,
    user_id: UserId,
    secret: &str,
    account_id: AccountId,
    debug_session_id: DebugSessionId,
) -> Result<Option<DebugClientConfig>, TError> {
    let url = Url::parse(&format!(
        "https://{server_hostname}/cli_debug_client_config?debug_session_id={debug_session_id}"
    ))?;

    eprintln!("Finding runner for debug session...");

    let response = reqwest::Client::new()
        .get(url)
        .header("x-muzanci-user-id", user_id.to_string())
        .header("x-muzanci-personal-access-token-secret", secret)
        .header("x-muzanci-account-id", account_id.to_string())
        .header("accept", "application/json")
        .send()
        .await?;

    if !response.status().is_success() {
        let body = response.text().await?;
        return Err(TError::new(&format!(
            "failed to fetch debug config: {body}"
        )));
    }

    let json = response.json::<CliGetDebugClientConfigResponse>().await?;

    Ok(json.debug_client_config_opt)
}

pub enum DebugClientInteractiveCommand {
    /// Execute the step at the given index in the job.
    Exec { step_idx: usize },

    /// Execute the step at the given index in the job and continue executing until a step returns a non-zero exit code.
    ExecContinue { step_idx: usize },

    /// Start an interactive shell in the job.
    Shell,

    /// Move the interactive debug session cursor to a specific step in the job.
    Move { step_idx: usize },

    /// Quit the interactive debug session.
    Quit,
}

#[derive(Debug, thiserror::Error)]
#[error("DebugClientError: {0}")]
struct DebugClientError(String);

struct DebugClient {
    client_shutdown: CancellationToken,
    channel_tx: ChannelSender,
    channel_rx: ChannelReceiver,
    channel_closed: CancellationToken,
    mux_handle: MuxHandle,
    user_id: UserId,
    account_id: AccountId,
    debug_client_config: DebugClientConfig,
}

impl DebugClient {
    async fn connect(
        client_shutdown: CancellationToken,
        server_hostname: &str,
        user_id: UserId,
        secret: &str,
        account_id: AccountId,
        debug_client_config: DebugClientConfig,
    ) -> Result<Self, TError> {
        let mux_handle = debug_client_mux_handle(
            client_shutdown.clone(),
            server_hostname,
            debug_client_config.server_id,
            user_id,
            secret,
            account_id,
        )
        .await?;

        let (channel_tx, mut channel_rx, channel_closed) =
            mux_handle.open_channel(ChannelType::DebugClient).await?;

        channel_tx
            .send(Message::DebugClient(
                DebugClientMessage::ConnectDebugClientRequest {
                    debug_session_id: debug_client_config.debug_session_id,
                },
            ))
            .await?;

        channel_rx
            .recv()
            .await
            .ok_or_else(|| DebugClientError("Channel closed".to_string()))
            .and_then(|response| match response {
                Message::DebugClient(DebugClientMessage::ConnectDebugClientResponse { result }) => {
                    result.map_err(|e| DebugClientError(e.to_string()))
                }
                _ => Err(DebugClientError("Unexpected message type".to_string())),
            })?;

        Ok(Self {
            client_shutdown,
            channel_tx,
            channel_rx,
            channel_closed,
            mux_handle,
            user_id,
            account_id,
            debug_client_config,
        })
    }

    async fn prepare_sandbox(
        &mut self,
        image: ImageConfig,
        git_remote: GitRemote,
    ) -> Result<(), TError> {
        //
        // Create sandbox
        //
        self.channel_tx
            .send(Message::DebugClient(
                DebugClientMessage::CreateSandboxRequest { image },
            ))
            .await?;

        self.channel_rx
            .recv()
            .await
            .ok_or_else(|| DebugClientError("Channel closed".to_string()))
            .and_then(|response| match response {
                Message::DebugClient(DebugClientMessage::CreateSandboxResponse { result }) => {
                    result.map_err(|e| DebugClientError(e.to_string()))
                }
                _ => Err(DebugClientError("Unexpected message type".to_string())),
            })?;

        //
        // Checkout git remote branch into sandbox
        //
        self.channel_tx
            .send(Message::DebugClient(
                DebugClientMessage::CheckoutBranchRequest {
                    url: git_remote.url,
                    branch: git_remote.branch.clone(),
                },
            ))
            .await?;

        self.channel_rx
            .recv()
            .await
            .ok_or_else(|| TError::new("Channel closed"))
            .and_then(|response| match response {
                Message::DebugClient(DebugClientMessage::CheckoutBranchResponse { result }) => {
                    result.map_err(|e| TError::new(&e.to_string()))
                }
                _ => Err(TError::new("Unexpected message type")),
            })?;

        //
        // Send local diff to sandbox
        //
        let diff_file = {
            let mut diff_file = tempfile::NamedTempFile::new()?;
            let git_client = GitClient::try_default()?;
            let target_dir = PathBuf::from("./.git");
            git_client.create_diff(&target_dir, git_remote.branch, &mut diff_file)?;
            diff_file
        };

        self.channel_tx
            .send(Message::DebugClient(
                DebugClientMessage::StartDiffUploadRequest,
            ))
            .await?;

        self.channel_rx
            .recv()
            .await
            .ok_or_else(|| TError::new("Channel closed"))
            .and_then(|response| match response {
                Message::DebugClient(DebugClientMessage::StartDiffUploadResponse { result }) => {
                    result.map_err(|e| TError::new(&e.to_string()))
                }
                _ => Err(TError::new("Unexpected message type")),
            })?;

        let diff_file = tokio::fs::File::from_std(diff_file.reopen()?);

        use sha2::Digest;
        use tokio::io::AsyncBufReadExt;

        const CHUNK_SIZE: usize = 1024 * 1024; // 1 MB
        let mut reader = BufReader::with_capacity(CHUNK_SIZE, diff_file);
        let mut diff_hasher = Sha256::new();

        loop {
            let buffer = reader.fill_buf().await?;
            let n = buffer.len();
            if n == 0 {
                break;
            }

            diff_hasher.update(buffer);

            let chunk = buffer.to_vec();
            self.channel_tx
                .send(Message::DebugClient(
                    DebugClientMessage::UploadDiffChunkRequest { chunk },
                ))
                .await?;

            self.channel_rx
                .recv()
                .await
                .ok_or_else(|| TError::new("Channel closed"))
                .and_then(|response| match response {
                    Message::DebugClient(DebugClientMessage::UploadDiffChunkResponse {
                        result,
                    }) => result.map_err(|e| TError::new(&e.to_string())),
                    _ => Err(TError::new("Unexpected message type")),
                })?;

            reader.consume(n);
        }

        let checksum = hex::encode(diff_hasher.finalize_reset());

        self.channel_tx
            .send(Message::DebugClient(
                DebugClientMessage::CompleteDiffUploadRequest { checksum },
            ))
            .await?;

        self.channel_rx
            .recv()
            .await
            .ok_or_else(|| TError::new("Channel closed"))
            .and_then(|response| match response {
                Message::DebugClient(DebugClientMessage::CompleteDiffUploadResponse { result }) => {
                    result.map_err(|e| TError::new(&e.to_string()))
                }
                _ => Err(TError::new("Unexpected message type")),
            })?;

        //
        // Apply local diff to sandbox
        //
        self.channel_tx
            .send(Message::DebugClient(DebugClientMessage::ApplyDiffRequest))
            .await?;

        self.channel_rx
            .recv()
            .await
            .ok_or_else(|| TError::new("Channel closed"))
            .and_then(|response| match response {
                Message::DebugClient(DebugClientMessage::ApplyDiffResponse { result }) => {
                    result.map_err(|e| TError::new(&e.to_string()))
                }
                _ => Err(TError::new("Unexpected message type")),
            })?;

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn start_interactive_session(
        &mut self,
        signal_receiver_enabled: Arc<AtomicBool>,
        job: JobConfig,
    ) -> Result<(), TError> {
        let mut stdin = StdinStream::new(self.client_shutdown.clone());
        let mut current_step: usize = 0;
        let mut step_success: Vec<Option<bool>> = vec![None; job.steps.len()];

        loop {
            // print current state
            println!("Job: {}", job.name);
            println!("Steps:");
            for (i, step) in job.steps.iter().enumerate() {
                let status = if let Some(success) = step_success[i] {
                    if success { "✔" } else { "✘" }
                } else {
                    "?"
                };
                let cursor = if i == current_step { ">" } else { " " };
                println!(
                    "{} {} ({}) {}\n\t\"{}\"\n",
                    cursor,
                    status,
                    i + 1,
                    step.name,
                    step.command
                );
            }

            println!("You can open a (s)hell, (m)ove, (n)ext, (c)ontinue, or (q)uit.");

            print!("> ");
            use std::io::Write;
            std::io::stdout().flush()?;

            // read user input line
            let line = stdin.read_line().await?;

            // parse into command
            let mut parts = line.split_whitespace();
            let command = parts.next();
            let command = match command {
                Some(command) => match command {
                    "s" | "shell" => DebugClientInteractiveCommand::Shell,
                    "m" | "move" => {
                        if let Some(i) = parts.next().and_then(|s| s.parse::<usize>().ok()) {
                            DebugClientInteractiveCommand::Move { step_idx: i - 1 }
                        } else {
                            DebugClientInteractiveCommand::Move {
                                step_idx: current_step,
                            }
                        }
                    }
                    "c" | "continue" => {
                        if let Some(i) = parts.next().and_then(|s| s.parse::<usize>().ok()) {
                            DebugClientInteractiveCommand::ExecContinue { step_idx: i - 1 }
                        } else {
                            DebugClientInteractiveCommand::ExecContinue {
                                step_idx: current_step,
                            }
                        }
                    }
                    "n" | "next" => DebugClientInteractiveCommand::Exec {
                        step_idx: current_step,
                    },
                    "q" | "quit" => DebugClientInteractiveCommand::Quit,
                    _ => {
                        println!("Unknown command: {}", line.trim());
                        continue;
                    }
                },
                None => continue,
            };

            // execute command
            match command {
                DebugClientInteractiveCommand::Quit => break,
                DebugClientInteractiveCommand::Exec { step_idx } => {
                    println!("Executing next.");
                    // Execute the step pointed by current_step.
                    let step = job.steps.get(step_idx).unwrap().clone();
                    let exit_code = self
                        .exec(signal_receiver_enabled.clone(), &step.command)
                        .await?;
                    if exit_code == 0 {
                        step_success[step_idx] = Some(true);
                        if step_idx + 1 < job.steps.len() {
                            current_step = step_idx + 1;
                        }
                    } else {
                        step_success[current_step] = Some(false);
                        current_step = step_idx;
                    }
                }
                DebugClientInteractiveCommand::ExecContinue { step_idx } => {
                    println!("Executing continue on {}", step_idx);
                    // Starting with step_idx, execute each step until the end or a failure.
                    current_step = step_idx;
                    loop {
                        if current_step >= job.steps.len() {
                            break;
                        }

                        let step = job.steps.get(current_step).unwrap().clone();
                        let exit_code = self
                            .exec(signal_receiver_enabled.clone(), &step.command)
                            .await?;
                        if exit_code == 0 {
                            step_success[current_step] = Some(true);
                            if current_step + 1 < job.steps.len() {
                                current_step += 1;
                            } else if current_step + 1 == job.steps.len() {
                                break;
                            }
                        } else {
                            step_success[current_step] = Some(false);
                            // If a failure occurs, do not advance the current_step.
                            break;
                        }
                    }
                }
                DebugClientInteractiveCommand::Shell => {
                    // Open an interactive shell for the step pointed by step_idx.
                    let _exit_code = self
                        .shell(signal_receiver_enabled.clone(), &mut stdin)
                        .await?;
                }
                DebugClientInteractiveCommand::Move { step_idx } => {
                    println!("Executing move on {}", step_idx);
                    // Move current_step to step_idx.
                    if step_idx < job.steps.len() {
                        current_step = step_idx;
                    } else {
                        println!("Step {} is out of bounds.", step_idx);
                    }
                }
            }

            // update state
        }

        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn exec(
        &mut self,
        signal_receiver_enabled: Arc<AtomicBool>,
        cmd: &str,
    ) -> Result<u32, TError> {
        self.channel_tx
            .send(Message::DebugClient(DebugClientMessage::StartShellRequest))
            .await?;

        self.channel_rx
            .recv()
            .await
            .ok_or_else(|| TError::new("Channel closed"))
            .and_then(|response| match response {
                Message::DebugClient(DebugClientMessage::StartShellResponse { result }) => {
                    result.map_err(|e| TError::new(&e.to_string()))
                }
                _ => Err(TError::new("Unexpected message type")),
            })?;

        let tunnel = DebugClientSshTunnel::connect(
            signal_receiver_enabled,
            self.mux_handle.clone(),
            self.user_id,
            self.account_id,
            self.debug_client_config.debug_session_id,
        )
        .await?;

        tunnel.exec(cmd).await
    }

    #[tracing::instrument(skip_all)]
    async fn shell(
        &mut self,
        signal_receiver_enabled: Arc<AtomicBool>,
        stdin: &mut StdinStream,
    ) -> Result<u32, TError> {
        self.channel_tx
            .send(Message::DebugClient(DebugClientMessage::StartShellRequest))
            .await?;

        self.channel_rx
            .recv()
            .await
            .ok_or_else(|| TError::new("Channel closed"))
            .and_then(|response| match response {
                Message::DebugClient(DebugClientMessage::StartShellResponse { result }) => {
                    result.map_err(|e| TError::new(&e.to_string()))
                }
                _ => Err(TError::new("Unexpected message type")),
            })?;

        let tunnel = DebugClientSshTunnel::connect(
            signal_receiver_enabled,
            self.mux_handle.clone(),
            self.user_id,
            self.account_id,
            self.debug_client_config.debug_session_id,
        )
        .await?;

        tunnel.shell(stdin).await
    }
}

// async fn run_debug_session(
//     client_shutdown: CancellationToken,
//     user_id: UserId,
//     secret: String,
//     account_id: AccountId,
//     job: JobConfig,
//     git_remote: GitRemote,
// ) -> Result<(), TError> {
//     let server_hostname = "localhost:8002";

//     let capacity = 1;

//     let debug_client_config = run_debug_config_resolver(
//         client_shutdown.clone(),
//         server_hostname,
//         user_id,
//         &secret,
//         account_id,
//         capacity,
//     )
//     .await?;

//     let signal_receiver_enabled = Arc::new(AtomicBool::new(true));
//     SignalReceiver::spawn(client_shutdown.clone(), signal_receiver_enabled.clone());

//     run_debug_client(
//         client_shutdown,
//         signal_receiver_enabled,
//         server_hostname,
//         user_id,
//         &secret,
//         account_id,
//         debug_client_config,
//         job,
//         git_remote,
//     )
//     .await?;

//     tracing::info!("debug session ended");

//     // DebugClient::debugger_control spawns a StdinStream task that blocks on a read syscall and
//     //  will not return until stdin is flushed with a newline. To avoid blocking process exit,
//     //  we explicitly exit immediately after the debug session ends.
//     std::process::exit(0);
// }

// /// Connects to the server, creates a debug session, and waits for debug config resolution.
// ///
// /// Debug config resolution is necessary because the runner assigned to the debug session
// /// may not have the same [`ServerId`] as the server the client is connected to. When a
// /// runner is assigned to a debug session, the client will receive a debug config and must
// /// connect a debug client to the runner's [`ServerId`] to start the debug session.
// async fn run_debug_config_resolver(
//     client_shutdown: CancellationToken,
//     server_hostname: &str,
//     user_id: UserId,
//     secret: &str,
//     account_id: AccountId,
//     capacity: u64,
// ) -> Result<DebugClientConfig, TError> {
//     let debug_resolver_mux_handle = mux_handle(
//         client_shutdown.clone(),
//         server_hostname,
//         "/begin_debug_resolver",
//         &[
//             ("x-muzanci-user-id", &user_id.to_string()),
//             ("x-muzanci-personal-access-token-secret", secret),
//             ("x-muzanci-account-id", &account_id.to_string()),
//         ],
//     )
//     .await?;

//     let join_handle = DebugResolver::spawn(
//         client_shutdown.clone(),
//         debug_resolver_mux_handle,
//         user_id,
//         account_id,
//         capacity,
//     );

//     match join_handle.await {
//         Ok(Ok(debug_client_config)) => Ok(debug_client_config),
//         Ok(Err(e)) => Err(e),
//         Err(e) => Err(e.into()),
//     }
// }

// async fn run_debug_client(
//     client_shutdown: CancellationToken,
//     signal_receiver_enabled: Arc<AtomicBool>,
//     server_hostname: &str,
//     user_id: UserId,
//     secret: &str,
//     account_id: AccountId,
//     debug_client_config: DebugClientConfig,
//     job: JobConfig,
//     git_remote: GitRemote,
// ) -> Result<(), TError> {
//     let debug_resolver_mux_handle = mux_handle(
//         client_shutdown.clone(),
//         server_hostname,
//         "/begin_debug_client",
//         &[
//             (
//                 "x-muzanci-server-id",
//                 &debug_client_config.server_id.to_string(),
//             ),
//             ("x-muzanci-user-id", &user_id.to_string()),
//             ("x-muzanci-personal-access-token-secret", secret),
//             ("x-muzanci-account-id", &account_id.to_string()),
//         ],
//     )
//     .await?;

//     let join_handle = DebugClient::spawn(
//         client_shutdown,
//         signal_receiver_enabled,
//         debug_resolver_mux_handle,
//         user_id,
//         account_id,
//         debug_client_config.debug_session_id,
//         job,
//         git_remote,
//     );

//     match join_handle.await {
//         Ok(result) => result,
//         Err(e) => Err(e.into()),
//     }
// }

#[derive(Debug, thiserror::Error)]
#[error("DebugSubcommandMuxHandleError: {0}")]
struct DebugSubcommandMuxHandleError(String);

async fn debug_client_mux_handle(
    client_shutdown: CancellationToken,
    server_hostname: &str,
    server_id: ServerId,
    user_id: UserId,
    secret: &str,
    account_id: AccountId,
) -> Result<MuxHandle, TError> {
    eprintln!("{server_hostname}");
    let server_stream = {
        let stream = tokio::net::TcpStream::connect(&format!("localhost:8002")).await?;
        stream.set_nodelay(true)?;
        hyper_util::rt::TokioIo::new(stream)
    };

    eprintln!("Connected TCP");

    let (mut send_request, connection) =
        hyper::client::conn::http1::handshake(server_stream).await?;

    eprintln!("Started handshake");

    tokio::spawn(async move {
        if let Err(e) = connection.with_upgrades().await {
            eprintln!("Connection error: {:?}", e);
        }
    });

    let request = Request::builder()
        .method("POST")
        .uri("/cli_connect_debug_client")
        .header(http::header::HOST, server_hostname)
        .header(http::header::CONNECTION, "Upgrade")
        .header(http::header::UPGRADE, MUZANCI_TRANSPORT_V1)
        .header("x-muzanci-server-id", &server_id.to_string())
        .header("x-muzanci-user-id", &user_id.to_string())
        .header("x-muzanci-personal-access-token-secret", secret)
        .header("x-muzanci-account-id", &account_id.to_string())
        .body(http_body_util::Empty::<bytes::Bytes>::new())?;

    eprintln!("built request");

    let response = send_request.send_request(request).await?;

    eprintln!("Receieved response");

    match response.status() {
        StatusCode::SWITCHING_PROTOCOLS => {}
        StatusCode::BAD_REQUEST => {
            let body_bytes = response.collect().await?.to_bytes();
            let e = serde_json::from_slice::<DebugClientResponse>(&body_bytes)?.error;
            let e = DebugSubcommandMuxHandleError(format!("Client error: {e}"));
            return Err(e.into());
        }
        StatusCode::INTERNAL_SERVER_ERROR => {
            let body_bytes = response.collect().await?.to_bytes();
            let e = serde_json::from_slice::<DebugClientResponse>(&body_bytes)?.error;
            let e = DebugSubcommandMuxHandleError(format!("Server error: {e}"));
            return Err(e.into());
        }
        code => {
            let e = DebugSubcommandMuxHandleError(format!("Unexpected status code: {code}"));
            return Err(e.into());
        }
    }

    let (read_half, write_half) = {
        let hyper_upgraded_stream = hyper::upgrade::on(response).await?;
        let tokio_stream = hyper_util::rt::TokioIo::new(hyper_upgraded_stream);
        tokio::io::split(tokio_stream)
    };

    let channel_acceptor = FnChannelAcceptor::new(move |channel_id, channel_type| {
        panic!(
            "Client received request to open channel [{}] of type {:?}",
            channel_id, channel_type
        );
    });

    let (_join_handle, mux_handle) =
        Mux::spawn(read_half, write_half, channel_acceptor, client_shutdown);

    Ok(mux_handle)
}
