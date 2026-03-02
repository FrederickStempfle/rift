use once_cell::sync::Lazy;
use regex::Regex;

use crate::error::AppError;

static PROJECT_NAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9-]{1,64}$").expect("project name regex must compile"));
static SUBDOMAIN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9-]{1,63}$").expect("subdomain regex must compile"));
static BRANCH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z0-9._/-]{1,255}$").expect("branch regex must compile"));
static OUTPUT_DIR_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z0-9._/-]{1,255}$").expect("output dir regex must compile"));
static DOMAIN_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^([a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?\.)+[a-zA-Z]{2,}$")
        .expect("domain regex must compile")
});
static REPO_URL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+(?:\.git)?$")
        .expect("repo url regex must compile")
});

const RESERVED_SUBDOMAINS: &[&str] = &["api", "www", "admin", "static"];

pub fn ensure_no_null_bytes(value: &str, field: &'static str) -> Result<(), AppError> {
    if value.contains('\0') {
        return Err(AppError::BadRequest(format!("{field} contains null bytes")));
    }

    Ok(())
}

pub fn ensure_max_len(value: &str, max_len: usize, field: &'static str) -> Result<(), AppError> {
    if value.len() > max_len {
        return Err(AppError::BadRequest(format!(
            "{field} exceeds max length of {max_len}"
        )));
    }

    Ok(())
}

pub fn validate_project_name(value: &str) -> Result<(), AppError> {
    ensure_no_null_bytes(value, "project name")?;
    if !PROJECT_NAME_RE.is_match(value) {
        return Err(AppError::BadRequest(
            "project name must match ^[a-z0-9-]{1,64}$".into(),
        ));
    }

    Ok(())
}

pub fn validate_subdomain(value: &str) -> Result<(), AppError> {
    ensure_no_null_bytes(value, "subdomain")?;
    if !SUBDOMAIN_RE.is_match(value) {
        return Err(AppError::BadRequest(
            "subdomain must match ^[a-z0-9-]{1,63}$".into(),
        ));
    }

    if RESERVED_SUBDOMAINS.contains(&value) {
        return Err(AppError::BadRequest("subdomain is reserved".into()));
    }

    Ok(())
}

pub fn validate_repo_url(value: &str) -> Result<(), AppError> {
    ensure_no_null_bytes(value, "repo url")?;
    ensure_max_len(value, 512, "repo url")?;
    if !REPO_URL_RE.is_match(value) {
        return Err(AppError::BadRequest(
            "repo_url must match https://github.com/{owner}/{repo}".into(),
        ));
    }

    Ok(())
}

pub fn validate_domain(value: &str) -> Result<(), AppError> {
    ensure_no_null_bytes(value, "domain")?;
    ensure_max_len(value, 253, "domain")?;
    if !DOMAIN_RE.is_match(value) {
        return Err(AppError::BadRequest(
            "domain must be a valid hostname (e.g. example.com)".into(),
        ));
    }

    Ok(())
}

pub fn validate_branch(value: &str) -> Result<(), AppError> {
    ensure_no_null_bytes(value, "branch")?;
    ensure_max_len(value, 255, "branch")?;
    if value.starts_with('-') {
        return Err(AppError::BadRequest(
            "branch must not start with '-'".into(),
        ));
    }
    if value.contains("..") {
        return Err(AppError::BadRequest("branch must not contain '..'".into()));
    }
    if !BRANCH_RE.is_match(value) {
        return Err(AppError::BadRequest(
            "branch must match ^[A-Za-z0-9._/-]{1,255}$".into(),
        ));
    }
    Ok(())
}

pub fn validate_output_dir(value: &str) -> Result<(), AppError> {
    ensure_no_null_bytes(value, "output dir")?;
    ensure_max_len(value, 255, "output dir")?;
    if value.starts_with('/') {
        return Err(AppError::BadRequest(
            "output dir must be a relative path".into(),
        ));
    }
    if value.split('/').any(|segment| segment == "..") {
        return Err(AppError::BadRequest(
            "output dir must not contain '..' path traversal".into(),
        ));
    }
    if !OUTPUT_DIR_RE.is_match(value) {
        return Err(AppError::BadRequest(
            "output dir must match ^[A-Za-z0-9._/-]{1,255}$".into(),
        ));
    }
    Ok(())
}

pub fn validate_custom_command(value: &str, field: &'static str) -> Result<(), AppError> {
    ensure_no_null_bytes(value, field)?;
    ensure_max_len(value, 1024, field)?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(format!("{field} must not be empty")));
    }

    // Shell metacharacters are blocked so custom commands cannot chain,
    // redirect, or execute subshell expressions.
    let forbidden = [
        ';', '|', '&', '>', '<', '`', '$', '(', ')', '{', '}', '\n', '\r',
    ];
    if trimmed.chars().any(|c| forbidden.contains(&c)) {
        return Err(AppError::BadRequest(format!(
            "{field} contains forbidden shell metacharacters"
        )));
    }

    Ok(())
}

pub fn validate_email(value: &str) -> Result<(), AppError> {
    ensure_no_null_bytes(value, "email")?;
    ensure_max_len(value, 320, "email")?;

    let parts: Vec<&str> = value.split('@').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(AppError::BadRequest("invalid email format".into()));
    }

    Ok(())
}

pub fn validate_password(value: &str) -> Result<(), AppError> {
    ensure_no_null_bytes(value, "password")?;
    if value.chars().count() < 12 {
        return Err(AppError::BadRequest(
            "password must be at least 12 characters".into(),
        ));
    }
    ensure_max_len(value, 4096, "password")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_project_name_passes() {
        assert!(validate_project_name("my-app-1").is_ok());
    }

    #[test]
    fn invalid_project_name_fails() {
        assert!(validate_project_name("My App").is_err());
    }

    #[test]
    fn reserved_subdomain_fails() {
        assert!(validate_subdomain("api").is_err());
    }

    #[test]
    fn valid_repo_url_passes() {
        assert!(validate_repo_url("https://github.com/org/repo").is_ok());
    }

    #[test]
    fn invalid_repo_url_fails() {
        assert!(validate_repo_url("git@github.com:org/repo").is_err());
    }

    #[test]
    fn valid_branch_passes() {
        assert!(validate_branch("feature/my-branch").is_ok());
    }

    #[test]
    fn invalid_branch_fails() {
        assert!(validate_branch("../../../etc/passwd").is_err());
        assert!(validate_branch("-main").is_err());
    }

    #[test]
    fn valid_output_dir_passes() {
        assert!(validate_output_dir("apps/web/dist").is_ok());
    }

    #[test]
    fn invalid_output_dir_fails() {
        assert!(validate_output_dir("/tmp/dist").is_err());
        assert!(validate_output_dir("../dist").is_err());
    }

    #[test]
    fn custom_command_blocks_metacharacters() {
        assert!(validate_custom_command("npm run build", "build command").is_ok());
        assert!(validate_custom_command("npm run build; rm -rf /", "build command").is_err());
    }

    #[test]
    fn short_password_fails() {
        assert!(validate_password("short").is_err());
    }

    #[test]
    fn email_requires_at_symbol() {
        assert!(validate_email("user.example.com").is_err());
    }
}
