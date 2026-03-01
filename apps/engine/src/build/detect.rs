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
    Nuxt,
    AstroSSR,
    SvelteKitSSR,
    RemixSSR,
    Static { dir: String },
    /// Serverless functions only (rift/functions/ directory, no framework).
    Functions,
}

#[derive(Clone, Debug)]
pub struct BuildPlan {
    pub framework: String,
    pub package_manager: PackageManager,
    pub install_command: String,
    pub build_command: String,
    pub output: BuildOutput,
}

/// Known web frameworks that produce deployable output.
/// Order matters: more specific frameworks (that also depend on vite) must come
/// before "vite" so they match first via `find`.
const WEB_FRAMEWORKS: &[&str] = &[
    "next",
    "nuxt",
    "@remix-run/dev",
    "astro",
    "@sveltejs/kit",
    "vite",
];

/// Platform-specific adapters that produce output formats we cannot run.
const UNSUPPORTED_ASTRO_ADAPTERS: &[(&str, &str)] = &[
    ("@astrojs/vercel", "Vercel"),
    ("@astrojs/cloudflare", "Cloudflare"),
    ("@astrojs/netlify", "Netlify"),
];

const UNSUPPORTED_SVELTEKIT_ADAPTERS: &[(&str, &str)] = &[
    ("@sveltejs/adapter-vercel", "Vercel"),
    ("@sveltejs/adapter-cloudflare", "Cloudflare"),
    ("@sveltejs/adapter-netlify", "Netlify"),
];

/// A workspace package that looks like a deployable web app.
#[derive(Debug)]
struct WorkspaceApp {
    /// The package name from package.json (e.g. "@lifo-sh/playground").
    name: String,
    /// Relative path from workspace root (e.g. "apps/playground").
    rel_path: String,
    /// The detected web framework dependency.
    framework: String,
    /// Whether the package has Next.js.
    is_next: bool,
    /// Whether the package has Nuxt.
    is_nuxt: bool,
    /// Whether the package has Astro with @astrojs/node adapter.
    is_astro_ssr: bool,
    /// Whether the package has SvelteKit with adapter-node.
    is_sveltekit_ssr: bool,
    /// Whether the package has Remix.
    is_remix: bool,
    /// If set, the app uses a platform-specific adapter we cannot deploy.
    unsupported_platform: Option<String>,
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

    // Check if this is a monorepo with a deployable web app
    let is_monorepo =
        workspace_dir.join("pnpm-workspace.yaml").exists() || parsed.get("workspaces").is_some();

    if is_monorepo {
        if let Some(app) = find_deployable_app(workspace_dir) {
            // Use package manager filters with dependency syntax (`...`)
            // to build the target AND its workspace dependencies. This bypasses
            // turbo pipelines which may include non-essential steps (typecheck,
            // lint) that can fail and block the build.
            let build_command =
                project
                    .build_command
                    .clone()
                    .unwrap_or_else(|| match package_manager {
                        PackageManager::Pnpm => format!("pnpm --filter {}... run build", app.name),
                        PackageManager::Yarn => format!("yarn workspace {} build", app.name),
                        PackageManager::Bun => format!("bun run --filter {} build", app.name),
                        PackageManager::Npm => format!("npm run build --workspace={}", app.name),
                    });

            if app.is_next {
                return Ok(BuildPlan {
                    framework: "nextjs".to_owned(),
                    package_manager,
                    install_command,
                    build_command,
                    output: BuildOutput::Next,
                });
            }

            if app.is_nuxt {
                return Ok(BuildPlan {
                    framework: "nuxt".to_owned(),
                    package_manager,
                    install_command,
                    build_command,
                    output: BuildOutput::Nuxt,
                });
            }

            if app.is_astro_ssr {
                return Ok(BuildPlan {
                    framework: "astro".to_owned(),
                    package_manager,
                    install_command,
                    build_command,
                    output: BuildOutput::AstroSSR,
                });
            }

            if app.is_sveltekit_ssr {
                return Ok(BuildPlan {
                    framework: "sveltekit".to_owned(),
                    package_manager,
                    install_command,
                    build_command,
                    output: BuildOutput::SvelteKitSSR,
                });
            }

            if app.is_remix {
                return Ok(BuildPlan {
                    framework: "remix".to_owned(),
                    package_manager,
                    install_command,
                    build_command,
                    output: BuildOutput::RemixSSR,
                });
            }

            // Reject apps that use platform-specific adapters we can't run
            if let Some(platform) = &app.unsupported_platform {
                return Err(AppError::BadRequest(format!(
                    "This project uses a {platform}-specific adapter which produces output that cannot be deployed here. \
                     For Astro, switch to @astrojs/node. For SvelteKit, switch to @sveltejs/adapter-node or @sveltejs/adapter-static."
                )));
            }

            return Ok(BuildPlan {
                framework: app.framework.clone(),
                package_manager,
                install_command,
                build_command,
                output: BuildOutput::Static {
                    dir: format!("{}/dist", app.rel_path),
                },
            });
        }
    }

    // Non-monorepo or no deployable app found in workspace — fall through to
    // standard single-package detection.
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

    let all_deps = dependencies.iter().chain(dev_dependencies.iter()).copied().collect::<Vec<_>>();

    let looks_like_next = all_deps.iter().any(|dep| *dep == "next")
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

    let looks_like_nuxt = all_deps.iter().any(|dep| *dep == "nuxt");

    if looks_like_nuxt {
        return Ok(BuildPlan {
            framework: "nuxt".to_owned(),
            package_manager,
            install_command,
            build_command,
            output: BuildOutput::Nuxt,
        });
    }

    // Astro with @astrojs/node adapter → SSR
    let has_astro = all_deps.iter().any(|dep| *dep == "astro");
    let has_astro_node = all_deps.iter().any(|dep| *dep == "@astrojs/node");
    if has_astro && has_astro_node {
        return Ok(BuildPlan {
            framework: "astro".to_owned(),
            package_manager,
            install_command,
            build_command,
            output: BuildOutput::AstroSSR,
        });
    }

    // Astro with platform-specific adapter → reject early
    if has_astro {
        if let Some(platform) = check_unsupported_adapter(&all_deps, UNSUPPORTED_ASTRO_ADAPTERS) {
            return Err(AppError::BadRequest(format!(
                "This project uses @astrojs/{} which produces output that cannot be deployed here. \
                 Switch to @astrojs/node for SSR or remove the adapter for static output.",
                platform.to_lowercase()
            )));
        }
    }

    // SvelteKit with adapter-node → SSR
    let has_sveltekit = all_deps.iter().any(|dep| *dep == "@sveltejs/kit");
    let has_adapter_node = all_deps.iter().any(|dep| *dep == "@sveltejs/adapter-node");
    if has_sveltekit && has_adapter_node {
        return Ok(BuildPlan {
            framework: "sveltekit".to_owned(),
            package_manager,
            install_command,
            build_command,
            output: BuildOutput::SvelteKitSSR,
        });
    }

    // SvelteKit with platform-specific adapter → reject early
    if has_sveltekit {
        if let Some(platform) = check_unsupported_adapter(&all_deps, UNSUPPORTED_SVELTEKIT_ADAPTERS) {
            return Err(AppError::BadRequest(format!(
                "This project uses @sveltejs/adapter-{} which produces output that cannot be deployed here. \
                 Switch to @sveltejs/adapter-node for SSR or @sveltejs/adapter-static for static output.",
                platform.to_lowercase()
            )));
        }
    }

    // Remix → always SSR
    let has_remix = all_deps
        .iter()
        .any(|dep| *dep == "@remix-run/dev" || *dep == "@remix-run/react" || *dep == "@react-router/dev");
    if has_remix {
        return Ok(BuildPlan {
            framework: "remix".to_owned(),
            package_manager,
            install_command,
            build_command,
            output: BuildOutput::RemixSSR,
        });
    }

    // Detect framework from known web framework dependencies
    let framework = WEB_FRAMEWORKS
        .iter()
        .find(|&&fw| all_deps.iter().any(|dep| *dep == fw))
        .map(|&fw| if fw == "next" { "nextjs" } else { fw }.to_owned())
        .unwrap_or_else(|| project.framework.clone());

    // Check for serverless functions directory (rift/functions/)
    // If no framework was detected but functions exist, treat as functions-only project
    if crate::build::functions::has_functions(workspace_dir)
        && framework == project.framework
        && !all_deps.iter().any(|dep| WEB_FRAMEWORKS.iter().any(|fw| dep == fw))
    {
        return Ok(BuildPlan {
            framework: "functions".to_owned(),
            package_manager,
            install_command,
            build_command,
            output: BuildOutput::Functions,
        });
    }

    Ok(BuildPlan {
        framework,
        package_manager,
        install_command,
        build_command,
        output: BuildOutput::Static { dir: String::new() },
    })
}

/// Detect the build output directory. Must be called AFTER the build completes
/// so that output directories (dist/, build/, out/) actually exist on disk.
pub fn detect_output_dir(project: &Project, workspace_dir: &Path) -> String {
    if let Some(dir) = project.output_dir.clone() {
        return dir;
    }

    const OUTPUT_DIRS: &[&str] = &["dist", "build", "out", ".output/public"];

    // Check root-level output dirs that contain an index.html (web app output)
    for dir in OUTPUT_DIRS {
        let candidate = workspace_dir.join(dir);
        if candidate.join("index.html").exists() {
            return (*dir).to_owned();
        }
    }

    // Scan apps/ first (preferred), then packages/, looking for web output with index.html
    for container in ["apps", "packages", "sites"] {
        for pkg_dir in list_subdirs(&workspace_dir.join(container)) {
            let pkg_name = pkg_dir
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            for output in OUTPUT_DIRS {
                let candidate = pkg_dir.join(output);
                if candidate.join("index.html").exists() {
                    return format!("{container}/{pkg_name}/{output}");
                }
            }
        }
    }

    // Fallback: any output dir with index.html at any depth
    for depth1 in list_subdirs(workspace_dir) {
        let d1_name = depth1
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        for output in OUTPUT_DIRS {
            let candidate = depth1.join(output);
            if candidate.join("index.html").exists() {
                return format!("{d1_name}/{output}");
            }
        }
        for depth2 in list_subdirs(&depth1) {
            let d2_name = depth2
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            for output in OUTPUT_DIRS {
                let candidate = depth2.join(output);
                if candidate.join("index.html").exists() {
                    return format!("{d1_name}/{d2_name}/{output}");
                }
            }
        }
    }

    // Last resort: any root output dir that exists (even without index.html)
    for dir in OUTPUT_DIRS {
        if workspace_dir.join(dir).exists() {
            return (*dir).to_owned();
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

/// Check if any unsupported platform-specific adapter is present in dependencies.
fn check_unsupported_adapter(all_deps: &[&str], adapters: &[(&str, &str)]) -> Option<String> {
    adapters
        .iter()
        .find(|(pkg, _)| all_deps.iter().any(|dep| dep == pkg))
        .map(|(_, platform)| (*platform).to_owned())
}

/// Scan workspace packages to find a deployable web app.
///
/// Looks in `apps/` (preferred) then `packages/` for a package that:
/// 1. Has a known web framework (vite, next, nuxt, etc.) as a dependency
/// 2. Has a `build` script
///
/// Prefers `apps/` over `packages/`, and within each container prefers
/// packages that have an `index.html` (SPA entry point).
fn find_deployable_app(workspace_dir: &Path) -> Option<WorkspaceApp> {
    let mut candidates = Vec::new();

    // Scan apps/ first, then packages/
    for container in ["apps", "packages", "sites"] {
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
            let pkg_json_path = entry.path().join("package.json");
            let Ok(content) = fs::read_to_string(&pkg_json_path) else {
                continue;
            };
            let Ok(parsed) = serde_json::from_str::<Value>(&content) else {
                continue;
            };

            // Must have a build script
            let has_build = parsed
                .get("scripts")
                .and_then(Value::as_object)
                .and_then(|s| s.get("build"))
                .is_some();
            if !has_build {
                continue;
            }

            // Check for web framework dependencies
            let all_deps: Vec<&str> = ["dependencies", "devDependencies"]
                .iter()
                .flat_map(|section| {
                    parsed
                        .get(*section)
                        .and_then(Value::as_object)
                        .into_iter()
                        .flatten()
                        .map(|(k, _)| k.as_str())
                })
                .collect();

            let framework = WEB_FRAMEWORKS
                .iter()
                .find(|&&fw| all_deps.iter().any(|dep| *dep == fw));

            if let Some(&fw) = framework {
                let dir_name_str = entry.file_name().to_string_lossy().to_string();
                let pkg_name = parsed
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|s| s.to_owned())
                    .unwrap_or_else(|| dir_name_str.clone());

                let has_index_html = entry.path().join("index.html").exists();

                let is_astro_ssr =
                    fw == "astro" && all_deps.iter().any(|dep| *dep == "@astrojs/node");
                let is_sveltekit_ssr = fw == "@sveltejs/kit"
                    && all_deps.iter().any(|dep| *dep == "@sveltejs/adapter-node");
                let is_remix = fw == "@remix-run/dev"
                    || all_deps.iter().any(|dep| {
                        *dep == "@remix-run/react" || *dep == "@react-router/dev"
                    });

                let unsupported_platform = if fw == "astro" && !is_astro_ssr {
                    check_unsupported_adapter(&all_deps, UNSUPPORTED_ASTRO_ADAPTERS)
                } else if fw == "@sveltejs/kit" && !is_sveltekit_ssr {
                    check_unsupported_adapter(&all_deps, UNSUPPORTED_SVELTEKIT_ADAPTERS)
                } else {
                    None
                };

                candidates.push((
                    WorkspaceApp {
                        name: pkg_name,
                        rel_path: format!("{container}/{dir_name_str}"),
                        framework: if fw == "next" {
                            "nextjs"
                        } else if fw == "@sveltejs/kit" {
                            "sveltekit"
                        } else if fw == "@remix-run/dev" {
                            "remix"
                        } else {
                            fw
                        }
                        .to_owned(),
                        is_next: fw == "next",
                        is_nuxt: fw == "nuxt",
                        is_astro_ssr,
                        is_sveltekit_ssr,
                        is_remix,
                        unsupported_platform,
                    },
                    container == "apps",
                    has_index_html,
                ));
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // Sort: prefer apps/ over packages/, then prefer those with index.html
    candidates.sort_by(|a, b| {
        b.1.cmp(&a.1) // apps/ first
            .then(b.2.cmp(&a.2)) // has index.html first
    });

    Some(candidates.into_iter().next().unwrap().0)
}
