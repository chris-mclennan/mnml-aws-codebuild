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
        let tab = self.active();
        if let TabData::Builds(b) = &tab.data
            && let Some(rec) = b.items.get(b.selected)
            && let Some(url) = rec.logs_deep_link.as_deref()
        {
            match webbrowser::open(url) {
                Ok(()) => self.status = format!("opened {url}"),
                Err(e) => self.status = format!("open failed: {e}"),
            }
        }
    }
}
