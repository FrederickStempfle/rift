use colored::Colorize;

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    UserMessage(String),

    #[error("Not authenticated. Run `rift login` first.")]
    NotAuthenticated,

    #[error("No project linked. Run `rift link` or pass --project.")]
    NoProject,

    #[error("Session expired. Run `rift login` to re-authenticate.")]
    SessionExpired,

    #[error("API error ({status}): {message}")]
    Api { status: u16, message: String },

    #[error("{0}")]
    Network(#[from] reqwest::Error),

    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Json(#[from] serde_json::Error),
}

pub fn print_error(e: &CliError) {
    eprintln!("{} {e}", "Error:".red().bold());
}
