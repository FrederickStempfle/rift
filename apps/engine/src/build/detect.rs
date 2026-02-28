use std::{fs, path::Path};

use serde_json::Value;

use crate::{db::models::Project, error::AppError};

#[derive(Clone, Debug)]
pub enum PackageManager {
    Npm,
    Pnpm,
    Bun,
}

#[derive(Clone, Debug)]
pub enum BuildOutput {
    Next,
    Static { dir: String },
}

#[derive(Clone, Debug)]
pub struct BuildPlan {
    pub framework: String,
    pub package_manager: PackageManager,
    pub install_command: String,
    pub build_command: String,
    pub output: BuildOutput,
}

pub fn detect_build_plan(project: &Project, workspace_dir: &Path) -> Result<BuildPlan, AppError> {
    let package_json_path = workspace_dir.join("package.json");
    let package_json = fs::read_to_string(&package_json_path).map_err(|error| {
        AppError::Internal(format!(
            "failed to read package.json from {}: {error}",
            package_json_path.display()
        ))
    })?;
    let parsed: Value = serde_json::from_str(&package_json)
        .map_err(|error| AppError::Internal(format!("invalid package.json: {error}")))?;

    let scripts = parsed.get("scripts").and_then(Value::as_object);
    let dependencies = parsed
        .get("dependencies")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>();
    let dev_dependencies = parsed
        .get("devDependencies")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>();

    let package_manager = if workspace_dir.join("pnpm-lock.yaml").exists() {
        PackageManager::Pnpm
    } else if workspace_dir.join("bun.lock").exists() || workspace_dir.join("bun.lockb").exists() {
        PackageManager::Bun
    } else {
        PackageManager::Npm
    };

    let install_command =
        project
            .install_command
            .clone()
            .unwrap_or_else(|| match package_manager {
                PackageManager::Pnpm => {
                    if workspace_dir.join("pnpm-lock.yaml").exists() {
                        "pnpm install --frozen-lockfile".to_owned()
                    } else {
                        "pnpm install".to_owned()
                    }
                }
                PackageManager::Bun => "bun install".to_owned(),
                PackageManager::Npm => {
                    if workspace_dir.join("package-lock.json").exists() {
                        "npm ci".to_owned()
                    } else {
                        "npm install".to_owned()
                    }
                }
            });

    let build_command = project
        .build_command
        .clone()
        .or_else(|| {
            scripts
                .and_then(|scripts| scripts.get("build"))
                .map(|_| match package_manager {
                    PackageManager::Pnpm => "pnpm build".to_owned(),
                    PackageManager::Bun => "bun run build".to_owned(),
                    PackageManager::Npm => "npm run build".to_owned(),
                })
        })
        .ok_or_else(|| AppError::BadRequest("project repo has no build command".into()))?;

    let looks_like_next = dependencies
        .iter()
        .chain(dev_dependencies.iter())
        .any(|dep| *dep == "next")
        || workspace_dir.join("next.config.js").exists()
        || workspace_dir.join("next.config.ts").exists()
        || workspace_dir.join("next.config.mjs").exists();

    if looks_like_next {
        return Ok(BuildPlan {
            framework: "nextjs".to_owned(),
            package_manager,
            install_command,
            build_command,
            output: BuildOutput::Next,
        });
    }

    let static_dir = project
        .output_dir
        .clone()
        .or_else(|| {
            ["dist", "build", "out"]
                .iter()
                .find(|dir| workspace_dir.join(dir).exists())
                .map(|dir| (*dir).to_owned())
        })
        .unwrap_or_else(|| "dist".to_owned());

    Ok(BuildPlan {
        framework: project.framework.clone(),
        package_manager,
        install_command,
        build_command,
        output: BuildOutput::Static { dir: static_dir },
    })
}
