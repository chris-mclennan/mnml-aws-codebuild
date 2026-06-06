//! App state — per-tab loaded data, the selection, the status string.

use crate::codebuild::{CodeBuildEvent, CodeBuildRecord, spawn_refresh};
use crate::config::{Config, Tab};
use crate::log_tail::{LogTailEvent, LogTailPane};
use anyhow::Result;
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabKind {
    Builds,
    Logs,
}

impl TabKind {
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "builds" => Ok(Self::Builds),
            "logs" => Ok(Self::Logs),
            other => Err(anyhow::anyhow!("unknown tab kind: {other}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TabSpec {
    pub kind: TabKind,
    pub region: Option<String>,
    pub project: Option<String>,
    pub log_group: Option<String>,
    pub log_stream: Option<String>,
}

impl TabSpec {
    pub fn resolve(t: &Tab, default_region: Option<&str>) -> Result<Self> {
        let kind = TabKind::from_str(&t.kind)?;
        let region = t
            .region
            .clone()
            .or_else(|| default_region.map(str::to_string));
        match kind {
            TabKind::Builds => {
                let project = t
                    .project
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("`project` required for kind `builds`"))?;
                Ok(Self {
                    kind,
                    region,
                    project: Some(project),
                    log_group: None,
                    log_stream: None,
                })
            }
            TabKind::Logs => {
                let log_group = t
                    .log_group
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("`log_group` required for kind `logs`"))?;
                Ok(Self {
                    kind,
                    region,
                    project: None,
                    log_group: Some(log_group),
                    log_stream: t.log_stream.clone(),
                })
            }
        }
    }
}

pub enum TabData {
    Builds(BuildsTab),
    Logs(LogsTab),
}

pub struct BuildsTab {
    pub items: Vec<CodeBuildRecord>,
    pub selected: usize,
    pub last_error: Option<String>,
    pub loading: bool,
    pub pending: Option<Receiver<CodeBuildEvent>>,
    pub last_fetched: Option<std::time::Instant>,
}

pub struct LogsTab {
    pub pane: Option<LogTailPane>,
    pub pending: Option<Receiver<LogTailEvent>>,
    pub last_error: Option<String>,
}

impl TabData {
    pub fn empty_for(kind: TabKind) -> Self {
        match kind {
            TabKind::Builds => Self::Builds(BuildsTab {
                items: Vec::new(),
                selected: 0,
                last_error: None,
                loading: false,
                pending: None,
                last_fetched: None,
            }),
            TabKind::Logs => Self::Logs(LogsTab {
                pane: None,
                pending: None,
                last_error: None,
            }),
        }
    }
}

pub struct TabState {
    pub name: String,
    pub spec: TabSpec,
    pub data: TabData,
}

pub struct App {
    pub cfg: Config,
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    pub status: String,
}

impl App {
    pub fn new(cfg: Config) -> Result<Self> {
        let mut tabs = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let spec = TabSpec::resolve(t, cfg.region.as_deref())?;
            tabs.push(TabState {
                name: t.name.clone(),
                data: TabData::empty_for(spec.kind),
                spec,
            });
        }
        let mut app = App {
            cfg,
            tabs,
            active_tab: 0,
            status: String::new(),
        };
        app.refresh_active();
        Ok(app)
    }

    pub fn active(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }
    pub fn active_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            self.refresh_active();
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let tab = self.active_mut();
        match &mut tab.data {
            TabData::Builds(b) => {
                if b.items.is_empty() {
                    return;
                }
                let n = b.items.len() as isize;
                let next = (b.selected as isize + delta).clamp(0, n - 1) as usize;
                b.selected = next;
            }
            TabData::Logs(l) => {
                if let Some(p) = l.pane.as_mut() {
                    if delta < 0 {
                        let n = (-delta) as usize;
                        if p.scroll == usize::MAX {
                            p.scroll = p.lines.len().saturating_sub(1);
                        }
                        p.scroll = p.scroll.saturating_sub(n);
                    } else {
                        let n = delta as usize;
                        let total = p.lines.len();
                        if p.scroll == usize::MAX || p.scroll.saturating_add(n) >= total {
                            p.scroll = usize::MAX;
                        } else {
                            p.scroll += n;
                        }
                    }
                }
            }
        }
    }

    pub fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let spec = self.tabs[idx].spec.clone();
        let name = self.tabs[idx].name.clone();
        match spec.kind {
            TabKind::Builds => {
                self.status = format!("refreshing {name}…");
                let pending = spawn_refresh(spec.project.unwrap_or_default(), spec.region);
                if let TabData::Builds(b) = &mut self.tabs[idx].data {
                    b.loading = true;
                    b.last_error = None;
                    b.pending = Some(pending);
                }
            }
            TabKind::Logs => {
                // Spawn only if not already running.
                let needs_spawn = matches!(
                    &self.tabs[idx].data,
                    TabData::Logs(l) if l.pane.is_none()
                );
                if needs_spawn {
                    self.status = format!("starting {name}…");
                    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
                    let res = LogTailPane::spawn(
                        spec.log_group.unwrap_or_default(),
                        spec.log_stream,
                        spec.region,
                        cwd,
                    );
                    if let TabData::Logs(l) = &mut self.tabs[idx].data {
                        match res {
                            Ok((pane, rx)) => {
                                l.pane = Some(pane);
                                l.pending = Some(rx);
                                l.last_error = None;
                            }
                            Err(e) => {
                                l.last_error = Some(e.clone());
                                self.status = format!("error: {e}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// Drain background channels — call from the main loop.
    pub fn drain(&mut self) -> bool {
        let mut any = false;
        for tab in self.tabs.iter_mut() {
            match &mut tab.data {
                TabData::Builds(b) => {
                    let Some(rx) = b.pending.take() else { continue };
                    let mut done = false;
                    loop {
                        match rx.try_recv() {
                            Ok(CodeBuildEvent::Builds(builds)) => {
                                any = true;
                                let n = builds.len();
                                b.items = builds;
                                b.last_error = None;
                                b.loading = false;
                                b.last_fetched = Some(std::time::Instant::now());
                                if b.selected >= b.items.len() {
                                    b.selected = b.items.len().saturating_sub(1);
                                }
                                done = true;
                                self.status = format!("{} · {n} builds", tab.name);
                            }
                            Ok(CodeBuildEvent::Failed(e)) => {
                                any = true;
                                b.last_error = Some(e.clone());
                                b.loading = false;
                                done = true;
                                self.status = format!("error: {e}");
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                done = true;
                                break;
                            }
                        }
                    }
                    if !done {
                        b.pending = Some(rx);
                    }
                }
                TabData::Logs(l) => {
                    let Some(rx) = l.pending.take() else { continue };
                    let mut still_open = true;
                    loop {
                        match rx.try_recv() {
                            Ok(LogTailEvent::Line(text)) => {
                                if let Some(p) = l.pane.as_mut() {
                                    use crate::log_tail::{LineSeverity, LogLine};
                                    let severity = LineSeverity::classify(&text);
                                    p.lines.push(LogLine { text, severity });
                                    if p.lines.len() > 5000 {
                                        let drop = p.lines.len() - 5000;
                                        p.lines.drain(0..drop);
                                    }
                                    any = true;
                                }
                            }
                            Ok(LogTailEvent::Failed(e)) => {
                                l.last_error = Some(e.clone());
                                self.status = format!("error: {e}");
                                still_open = false;
                                any = true;
                            }
                            Ok(LogTailEvent::Exited(_)) => {
                                still_open = false;
                                any = true;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => break,
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                still_open = false;
                                break;
                            }
                        }
                    }
                    if still_open {
                        l.pending = Some(rx);
                    }
                }
            }
        }
        any
    }

    pub fn open_focused(&mut self) {
        let Some(url) = self.focused_url() else {
            return;
        };
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {url}"),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }

    /// `y` on a focused build row — copy the CodeBuild console URL
    /// to the OS clipboard. Restores the pre-split mnml
    /// `aws.copy_selected_build_url` command (the CodeBuild console
    /// URL goes via `logs.deepLink`, which routes to the build
    /// detail page).
    pub fn yank_focused_url(&mut self) {
        let Some(url) = self.focused_url() else {
            self.status = "no URL for this row".to_string();
            return;
        };
        match crate::clipboard::copy(&url) {
            Ok(()) => self.status = format!("copied {url}"),
            Err(e) => self.status = format!("copy failed: {e}"),
        }
    }

    fn focused_url(&self) -> Option<String> {
        let tab = self.active();
        if let TabData::Builds(b) = &tab.data {
            return b.items.get(b.selected).and_then(|rec| rec.logs_deep_link.clone());
        }
        None
    }

    /// `L` on a build row — open an ephemeral Logs tab that tails the
    /// selected build's CloudWatch log stream (or switch to it if one
    /// already exists for that build). The stream name comes from the
    /// API response's `logs.streamName`; old builds without log
    /// metadata toast an explanation instead of opening a dead tab.
    pub fn open_logs_for_selected_build(&mut self) {
        let (build_id, group, stream, region) = {
            let tab = self.active();
            let TabData::Builds(b) = &tab.data else {
                self.status = "select a build row first (`L` works on a Builds tab)".to_string();
                return;
            };
            let Some(rec) = b.items.get(b.selected) else {
                self.status = "no build selected".to_string();
                return;
            };
            let (Some(group), Some(stream)) = (rec.logs_group.clone(), rec.logs_stream.clone())
            else {
                self.status =
                    format!("no log stream on build {} — CloudWatch metadata absent", rec.id);
                return;
            };
            (
                rec.id.clone(),
                group,
                stream,
                tab.spec.region.clone(),
            )
        };

        // Short label: trailing chunk of the build id (after the last
        // colon) is the per-run identifier; full id is too long for a
        // tab strip.
        let short = build_id.rsplit(':').next().unwrap_or(&build_id);
        let short = short.chars().take(8).collect::<String>();
        let tab_name = format!("{short} logs");

        // Switch to an existing matching tab if one's already open
        // (same stream — switching back to it re-uses the running
        // `aws logs tail`).
        if let Some(idx) = self.tabs.iter().position(|t| {
            t.spec.kind == TabKind::Logs
                && t.spec.log_stream.as_deref() == Some(stream.as_str())
                && t.spec.log_group.as_deref() == Some(group.as_str())
        }) {
            self.switch_tab(idx);
            self.status = format!("switched to {}", self.tabs[idx].name);
            return;
        }

        let spec = TabSpec {
            kind: TabKind::Logs,
            region,
            project: None,
            log_group: Some(group),
            log_stream: Some(stream),
        };
        self.tabs.push(TabState {
            name: tab_name.clone(),
            data: TabData::empty_for(TabKind::Logs),
            spec,
        });
        self.active_tab = self.tabs.len() - 1;
        self.refresh_active();
        self.status = format!("opened {tab_name}");
    }
}
