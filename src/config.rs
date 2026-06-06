//! Config file at `~/.config/mnml-aws-codebuild.toml`. First run
//! writes the scaffold + exits with instructions.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Default AWS region. Tabs can override via per-row `region = "..."`.
    /// `None` ⇒ defer to the `aws` CLI's own resolution
    /// (`AWS_REGION` / `AWS_DEFAULT_REGION` / config profile).
    #[serde(default)]
    pub region: Option<String>,
    /// Polling interval for `builds` tabs. `0` disables auto-refresh;
    /// user can still press `r`. Default 60s.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required.
    #[serde(default)]
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    /// Human label shown in the tab strip.
    pub name: String,
    /// `builds` — recent CodeBuild runs for `project`.
    /// `logs` — `aws logs tail --follow` on `log_group` + optional
    ///          `log_stream`.
    #[serde(default = "default_kind")]
    pub kind: String,
    /// Override the default region for this tab.
    #[serde(default)]
    pub region: Option<String>,
    /// CodeBuild project name. Required for `kind = "builds"`.
    #[serde(default)]
    pub project: Option<String>,
    /// CloudWatch log group name. Required for `kind = "logs"`.
    #[serde(default)]
    pub log_group: Option<String>,
    /// Optional log stream filter for `kind = "logs"`. None ⇒ tail every
    /// stream in the group.
    #[serde(default)]
    pub log_stream: Option<String>,
}

fn default_kind() -> String {
    "builds".to_string()
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-aws-codebuild config. Edit and re-run.
#
# `region` is optional — falls back to the `aws` CLI's own resolution
# ($AWS_REGION / $AWS_DEFAULT_REGION / your profile).
# region = "us-east-1"

# Auto-refresh in seconds. 0 disables; user can still press `r`.
refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Each `[[tabs]]` entry is one tab. Switched via 1-9 (or click) and
# rendered left→right.
#
# Supported kinds:
#   builds — recent CodeBuild runs for `project`
#   logs   — `aws logs tail --follow` on `log_group` (+ optional `log_stream`)

[[tabs]]
name    = "my-app builds"
project = "my-app"

[[tabs]]
name      = "my-app logs"
kind      = "logs"
log_group = "/aws/codebuild/my-app"

# Second project, narrowed to one stream.
# [[tabs]]
# name       = "playwright logs"
# kind       = "logs"
# log_group  = "/aws/codebuild/playwright-runner"
# log_stream = "abc123"
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            let valid_kind = matches!(t.kind.as_str(), "builds" | "logs");
            if !valid_kind {
                return Err(anyhow!(
                    "tab #{i} ({}): kind must be `builds` or `logs`, got `{}`",
                    t.name,
                    t.kind
                ));
            }
            match t.kind.as_str() {
                "builds" => {
                    let p = t.project.as_deref().unwrap_or("").trim();
                    if p.is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `project` is required for kind `builds`",
                            t.name
                        ));
                    }
                }
                "logs" => {
                    let g = t.log_group.as_deref().unwrap_or("").trim();
                    if g.is_empty() {
                        return Err(anyhow!(
                            "tab #{i} ({}): `log_group` is required for kind `logs`",
                            t.name
                        ));
                    }
                }
                _ => unreachable!("kind validity already checked above"),
            }
        }
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-aws-codebuild.toml")
}

pub fn load() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, Config::EXAMPLE)?;
        return Err(anyhow!(
            "wrote config template to {} — edit it then re-run",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(&path)?;
    let cfg: Config = toml::from_str(&text)?;
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses_and_validates() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).unwrap();
        cfg.validate().unwrap();
    }

    #[test]
    fn validate_rejects_builds_without_project() {
        let raw = r##"
[[tabs]]
name = "x"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_logs_without_log_group() {
        let raw = r##"
[[tabs]]
name = "x"
kind = "logs"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("log_group"));
    }

    #[test]
    fn validate_rejects_unknown_kind() {
        let raw = r##"
[[tabs]]
name = "x"
kind = "garbage"
project = "p"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(err.to_string().contains("builds"));
    }

    #[test]
    fn validate_rejects_no_tabs() {
        let cfg: Config = toml::from_str("refresh_interval_secs = 30").unwrap();
        assert!(cfg.validate().is_err());
    }
}
