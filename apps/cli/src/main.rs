mod client;
mod commands;
mod config;
mod credentials;
mod error;
mod output;

use clap::{Parser, Subcommand};

use client::RiftClient;
use error::CliError;

#[derive(Parser)]
#[command(name = "rift", version, about = "Rift CLI — deploy from anywhere")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Output JSON instead of formatted tables
    #[arg(long, global = true)]
    json: bool,

    /// Override the API URL from config
    #[arg(long, global = true)]
    api_url: Option<String>,

    /// Override the linked project (ID or name)
    #[arg(long, global = true)]
    project: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Log in to your Rift instance
    Login,

    /// Log out and clear credentials
    Logout,

    /// Show the current authenticated user
    Whoami,

    /// Manage projects
    Projects {
        #[command(subcommand)]
        action: Option<ProjectAction>,
    },

    /// Link current directory to a Rift project
    Link,

    /// Remove project link from current directory
    Unlink,

    /// Trigger a new deployment
    Deploy,

    /// List deployment history
    Deployments,

    /// Manage environment variables
    Env {
        #[command(subcommand)]
        action: EnvAction,
    },

    /// Manage custom domains
    Domains {
        #[command(subcommand)]
        action: DomainAction,
    },

    /// View deployment logs
    Logs {
        /// Specific deployment ID (defaults to latest)
        deployment_id: Option<String>,

        /// Stream logs in real time
        #[arg(short, long)]
        follow: bool,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    /// Create a new project
    Create,
    /// Show project details
    Info {
        /// Project ID or name
        project: Option<String>,
    },
    /// Delete a project
    Delete {
        /// Project ID or name
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum EnvAction {
    /// List environment variables
    List,
    /// Set an environment variable
    Set {
        /// Variable name
        key: String,
        /// Variable value
        value: String,
    },
    /// Remove an environment variable
    Unset {
        /// Variable name
        key: String,
    },
}

#[derive(Subcommand)]
enum DomainAction {
    /// List custom domains
    List,
    /// Add a custom domain
    Add {
        /// Domain name (e.g. app.example.com)
        domain: String,
    },
    /// Remove a custom domain
    Remove {
        /// Domain name
        domain: String,
    },
    /// Verify DNS and trigger SSL provisioning
    Verify {
        /// Domain name
        domain: String,
    },
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
    if let Err(e) = rt.block_on(run()) {
        error::print_error(&e);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), CliError> {
    let cli = Cli::parse();

    // Ensure config exists (prompts on first run)
    let mut cfg = commands::auth::ensure_config()?;

    // Override API URL if provided
    if let Some(url) = &cli.api_url {
        cfg.api_url = url.trim_end_matches('/').to_string();
    }

    let mut client = RiftClient::new(cfg);

    match cli.command {
        Command::Login => commands::auth::login(&mut client, cli.json).await,
        Command::Logout => commands::auth::logout(&mut client).await,
        Command::Whoami => commands::auth::whoami(&mut client, cli.json).await,

        Command::Projects { action } => match action {
            None => commands::projects::list(&mut client, cli.json).await,
            Some(ProjectAction::Create) => {
                commands::projects::create(&mut client, cli.json).await
            }
            Some(ProjectAction::Info { project }) => {
                let p = project.or(cli.project.clone());
                commands::projects::info(&mut client, &p, cli.json).await
            }
            Some(ProjectAction::Delete { project }) => {
                let p = project.or(cli.project.clone());
                commands::projects::delete(&mut client, &p).await
            }
        },

        Command::Link => commands::projects::link(&mut client).await,
        Command::Unlink => commands::projects::unlink().await,

        Command::Deploy => {
            commands::deploy::deploy(&mut client, &cli.project, cli.json).await
        }
        Command::Deployments => {
            commands::deploy::list(&mut client, &cli.project, cli.json).await
        }

        Command::Env { action } => match action {
            EnvAction::List => {
                commands::env::list(&mut client, &cli.project, cli.json).await
            }
            EnvAction::Set { key, value } => {
                commands::env::set(&mut client, &cli.project, &key, &value).await
            }
            EnvAction::Unset { key } => {
                commands::env::unset(&mut client, &cli.project, &key).await
            }
        },

        Command::Domains { action } => match action {
            DomainAction::List => {
                commands::domains::list(&mut client, &cli.project, cli.json).await
            }
            DomainAction::Add { domain } => {
                commands::domains::add(&mut client, &cli.project, &domain).await
            }
            DomainAction::Remove { domain } => {
                commands::domains::remove(&mut client, &cli.project, &domain).await
            }
            DomainAction::Verify { domain } => {
                commands::domains::verify(&mut client, &cli.project, &domain).await
            }
        },

        Command::Logs {
            deployment_id,
            follow,
        } => {
            commands::logs::logs(&mut client, &cli.project, deployment_id, follow, cli.json)
                .await
        }
    }
}
