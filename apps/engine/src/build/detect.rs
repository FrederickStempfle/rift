use std::{fs, path::Path};

use serde_json::Value;

use crate::{db::models::Project, error::AppError};

#[derive(Clone, Debug)]
pub enum PackageManager {
    Npm,
    Yarn,
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
    } else if workspace_dir.join("yarn.lock").exists() {
        PackageManager::Yarn
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
                PackageManager::Pnpm => "pnpm install".to_owned(),
                PackageManager::Yarn => "yarn install".to_owned(),
                PackageManager::Bun => "bun install".to_owned(),
                PackageManager::Npm => "npm install".to_owned(),
            });

    let build_command = project
        .build_command
        .clone()
        .or_else(|| {
            scripts
                .and_then(|scripts| scripts.get("build"))
                .map(|_| match package_manager {
                    PackageManager::Pnpm => "pnpm build".to_owned(),
                    PackageManager::Yarn => "yarn build".to_owned(),
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
        || workspace_dir.join("next.config.mjs").exists()
        || has_dep_in_workspace(workspace_dir, "next");

    if looks_like_next {
        return Ok(BuildPlan {
            framework: "nextjs".to_owned(),
            package_manager,
            install_command,
            build_command,
            output: BuildOutput::Next,
        });
    }

    let looks_like_vite = dependencies.iter().chain(dev_dependencies.iter()).any(|dep| *dep == "vite")
        || workspace_dir.join("vite.config.ts").exists()
        || workspace_dir.join("vite.config.js").exists()
        || workspace_dir.join("vite.config.mts").exists()
        || has_dep_in_workspace(workspace_dir, "vite");

    let framework = if looks_like_vite {
        "vite".to_owned()
    } else {
        project.framework.clone()
    };

    Ok(BuildPlan {
        framework,
        package_manager,
        install_command,
        build_command,
        output: BuildOutput::Static {
            dir: String::new(),
        },
    })
}

/// Detect the build output directory. Must be called AFTER the build completes
/// so that output directories (dist/, build/, out/) actually exist on disk.
pub fn detect_output_dir(project: &Project, workspace_dir: &Path) -> String {
    if let Some(dir) = project.output_dir.clone() {
        return dir;
    }

    const OUTPUT_DIRS: &[&str] = &["dist", "build", "out", ".output/public"];

    // Check root-level output dirs first
    for dir in OUTPUT_DIRS {
        if workspace_dir.join(dir).exists() {
            return (*dir).to_owned();
        }
    }

    // Scan up to 2 levels deep for monorepo structures
    // e.g. apps/web/dist, packages/app/build
    for depth1 in list_subdirs(workspace_dir) {
        let d1_name = depth1.file_name().unwrap_or_default().to_string_lossy().to_string();

        // Check depth-1 output dirs (e.g. web/dist)
        for output in OUTPUT_DIRS {
            if depth1.join(output).exists() {
                return format!("{d1_name}/{output}");
            }
        }

        // Check depth-2 output dirs (e.g. apps/web/dist, packages/app/build)
        for depth2 in list_subdirs(&depth1) {
            let d2_name = depth2.file_name().unwrap_or_default().to_string_lossy().to_string();
            for output in OUTPUT_DIRS {
                if depth2.join(output).exists() {
                    return format!("{d1_name}/{d2_name}/{output}");
                }
            }
        }
    }

    "dist".to_owned()
}

/// List subdirectories, skipping node_modules and hidden dirs.
fn list_subdirs(dir: &Path) -> Vec<std::path::PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) && {
                let name = e.file_name();
                let name_str = name.to_string_lossy();
                name_str != "node_modules" && !name_str.starts_with('.')
            }
        })
        .map(|e| e.path())
        .collect()
}

/// Check if any workspace package has a given dependency.
/// Scans package.json files in common monorepo locations (apps/*, packages/*).
fn has_dep_in_workspace(workspace_dir: &Path, dep_name: &str) -> bool {
    for container in ["apps", "packages"] {
        let container_dir = workspace_dir.join(container);
        if !container_dir.is_dir() {
            continue;
        }
        let Ok(entries) = fs::read_dir(&container_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let pkg_json = entry.path().join("package.json");
            if let Ok(content) = fs::read_to_string(&pkg_json) {
                if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
                    for section in ["dependencies", "devDependencies"] {
                        if parsed
                            .get(section)
                            .and_then(Value::as_object)
                            .map(|deps| deps.contains_key(dep_name))
                            .unwrap_or(false)
                        {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}
