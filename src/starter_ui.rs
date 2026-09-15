//! Terminal UI for the workspace starter.
//!
//! The starter is intentionally a separate pane from the manager.  The manager is a list of
//! everything installed; this view is a short-lived, confirmation-first workflow for one
//! workspace.  It never reads commands from a profile and it only offers actions/panes that
//! Herdr reported from an installed plugin manifest.

use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal;

use crate::starter::{self, ItemState, LaunchKind, LaunchTarget, Plan, Source, SourceKind};
use crate::Installed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Review,
    Launch,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Install,
    Update,
}

#[derive(Debug, Clone)]
struct LaunchChoice {
    target: LaunchTarget,
    picked: bool,
}

#[derive(Debug, Clone)]
struct LaunchResult {
    target: LaunchTarget,
    message: String,
    success: bool,
}

struct App {
    profile: Option<crate::profile::WorkspaceProfile>,
    profile_error: Option<String>,
    global_source: Source,
    profile_source: Option<Source>,
    selected_source: SourceKind,
    installed: Vec<Installed>,
    market: Vec<crate::registry::Entry>,
    plan: Option<Plan>,
    launch_choices: Vec<LaunchChoice>,
    stage: Stage,
    pending_operation: Option<Operation>,
    pending_launch: bool,
    results: Vec<LaunchResult>,
    cursor: usize,
    error: Option<String>,
    flash: Option<String>,
}

impl App {
    fn load() -> Self {
        let context = crate::context::read_context_from_env();
        let (profile, profile_error) = match crate::profile::discover(context.context.as_ref()) {
            Ok(profile) => (profile, None),
            Err(error) => (None, Some(error)),
        };
        let global_source = Source::global(
            crate::bundle_path(),
            crate::lock_path(),
            crate::read_bundle_specs().unwrap_or_default(),
        );
        let profile_source = profile.as_ref().map(|profile| {
            Source::profile(
                profile.list_path.clone(),
                profile.lock_path.clone(),
                profile.specs.clone(),
            )
        });
        let selected_source = if profile_source.is_some() || profile_error.is_some() {
            SourceKind::Profile
        } else {
            SourceKind::Global
        };
        let market = crate::registry::cached_entries();
        let (installed, error) = match crate::installed_plugins() {
            Ok(installed) => (installed, None),
            Err(error) => (Vec::new(), Some(error)),
        };

        let mut app = Self {
            profile,
            profile_error,
            global_source,
            profile_source,
            selected_source,
            installed,
            market,
            plan: None,
            launch_choices: Vec::new(),
            stage: Stage::Review,
            pending_operation: None,
            pending_launch: false,
            results: Vec::new(),
            cursor: 0,
            error,
            flash: None,
        };
        app.rebuild();
        app
    }

    fn rebuild(&mut self) {
        self.plan = if self.error.is_some() {
            None
        } else {
            self.active_source()
                .map(|source| starter::build_plan(source, &self.installed, &self.market))
        };
        self.launch_choices = self
            .plan
            .as_ref()
            .map(|plan| {
                starter::launch_targets(&plan.source.specs, &self.installed)
                    .into_iter()
                    .map(|target| LaunchChoice {
                        target,
                        picked: false,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let max = match self.stage {
            Stage::Launch => self.launch_choices.len(),
            _ => self.plan.as_ref().map(|plan| plan.items.len()).unwrap_or(0),
        };
        self.cursor = self.cursor.min(max.saturating_sub(1));
    }

    fn active_source(&self) -> Option<&Source> {
        match self.selected_source {
            SourceKind::Global => Some(&self.global_source),
            SourceKind::Profile => self.profile_source.as_ref(),
        }
    }

    fn source_available(&self) -> bool {
        self.active_source().is_some()
    }

    fn source_label(&self) -> &'static str {
        self.active_source()
            .map(Source::label)
            .unwrap_or(match self.selected_source {
                SourceKind::Profile => "workspace profile",
                SourceKind::Global => "global bundle",
            })
    }

    fn active_source_owned(&self) -> Option<Source> {
        self.active_source().cloned()
    }

    fn refresh(&mut self) {
        let selected = self.selected_source;
        let mut fresh = Self::load();
        fresh.selected_source = selected;
        if selected == SourceKind::Profile && !fresh.source_available() {
            fresh.selected_source = SourceKind::Global;
            fresh.flash =
                Some("workspace profile changed or disappeared — using global bundle".to_string());
        }
        fresh.stage = Stage::Review;
        fresh.pending_operation = None;
        fresh.pending_launch = false;
        fresh.results.clear();
        fresh.cursor = 0;
        fresh.rebuild();
        *self = fresh;
    }

    fn toggle_source(&mut self) {
        if self.profile.is_none() && self.profile_error.is_none() {
            self.flash = Some("no workspace profile found — using the global bundle".to_string());
            return;
        }
        self.selected_source = match self.selected_source {
            SourceKind::Profile => SourceKind::Global,
            SourceKind::Global => SourceKind::Profile,
        };
        self.stage = Stage::Review;
        self.cursor = 0;
        self.rebuild();
        self.flash = Some(format!("reviewing {}", self.source_label()));
    }

    fn candidate_count(&self, operation: Operation) -> usize {
        let Some(plan) = self.plan.as_ref() else {
            return 0;
        };
        plan.items
            .iter()
            .filter(|item| match operation {
                Operation::Install => item.state.needs_install(),
                Operation::Update => item.state.needs_update(),
            })
            .count()
    }

    fn request_operation(&mut self, operation: Operation) {
        if self.error.is_some() {
            self.flash = Some(
                self.error
                    .clone()
                    .unwrap_or_else(|| "cannot read installed plugins".to_string()),
            );
            return;
        }
        if !self.source_available() {
            self.flash = Some(
                self.profile_error
                    .clone()
                    .unwrap_or_else(|| "no valid workspace profile is available".to_string()),
            );
            return;
        }
        let count = self.candidate_count(operation);
        if count == 0 {
            self.flash = Some(match operation {
                Operation::Install => "nothing is missing or drifted".to_string(),
                Operation::Update => {
                    "no cached update candidates — refresh the marketplace from the manage pane with /"
                        .to_string()
                }
            });
            return;
        }
        self.pending_operation = Some(operation);
        self.flash = None;
    }

    fn current_source(&self) -> Result<Option<Source>, String> {
        match self.selected_source {
            SourceKind::Global => Ok(Some(Source::global(
                crate::bundle_path(),
                crate::lock_path(),
                crate::read_bundle_specs().unwrap_or_default(),
            ))),
            SourceKind::Profile => {
                let Some(profile) = self.profile.as_ref() else {
                    return Ok(None);
                };
                let current = crate::profile::discover_at(&profile.root)?;
                Ok(current.map(|profile| {
                    Source::profile(profile.list_path, profile.lock_path, profile.specs)
                }))
            }
        }
    }

    fn commit_operation(&mut self) -> io::Result<()> {
        let Some(operation) = self.pending_operation.take() else {
            return Ok(());
        };
        let Some(preview_source) = self.active_source_owned() else {
            self.flash = Some("the selected workspace profile is no longer available".to_string());
            return Ok(());
        };
        let current_source = match self.current_source() {
            Ok(Some(source)) => source,
            Ok(None) => {
                self.refresh();
                self.flash =
                    Some("workspace profile was removed — nothing was applied".to_string());
                return Ok(());
            }
            Err(error) => {
                self.refresh();
                self.flash = Some(format!("workspace profile is no longer valid: {}", error));
                return Ok(());
            }
        };
        if current_source != preview_source {
            self.refresh();
            self.flash =
                Some("the selected list changed — review the new starter plan first".to_string());
            return Ok(());
        }

        let update_targets: Vec<String> = self
            .plan
            .as_ref()
            .map(|plan| {
                plan.items
                    .iter()
                    .filter(|item| {
                        matches!(operation, Operation::Update) && item.state.needs_update()
                    })
                    .map(|item| item.spec.repo.clone())
                    .collect()
            })
            .unwrap_or_default();
        let specs = preview_source.specs.clone();
        let lock_path = preview_source.lock_path.clone();
        let mut result = None;
        crate::ui::suspended(|| {
            result = Some(match operation {
                Operation::Install => crate::sync_specs_to_lock(&specs, &lock_path),
                Operation::Update => {
                    let targets: Vec<&str> = update_targets.iter().map(String::as_str).collect();
                    crate::update_specs_to(&specs, &targets, &lock_path)
                }
            });
        })?;
        self.refresh();
        self.flash = Some(match result.expect("starter operation ran") {
            Ok(()) => match operation {
                Operation::Install => format!("{} install step completed", self.source_label()),
                Operation::Update => format!("{} update step completed", self.source_label()),
            },
            Err(error) => format!("starter operation failed: {}", error),
        });
        Ok(())
    }

    fn open_launch(&mut self) {
        if self.launch_choices.is_empty() {
            self.flash = Some(
                "no installed manifest-declared panes or actions are available for this selection"
                    .to_string(),
            );
            return;
        }
        self.stage = Stage::Launch;
        self.cursor = 0;
        self.flash = None;
    }

    fn request_launch(&mut self) {
        if !self.launch_choices.iter().any(|choice| choice.picked) {
            self.flash = Some("select at least one pane or action with space".to_string());
            return;
        }
        self.pending_launch = true;
        self.flash = None;
    }

    fn commit_launch(&mut self) {
        self.pending_launch = false;
        let Some(source) = self.active_source_owned() else {
            self.results = self
                .launch_choices
                .iter()
                .filter(|choice| choice.picked)
                .map(|choice| LaunchResult {
                    target: choice.target.clone(),
                    message: "skipped: selected profile is no longer available".to_string(),
                    success: false,
                })
                .collect();
            self.stage = Stage::Results;
            return;
        };
        let picked: Vec<LaunchTarget> = self
            .launch_choices
            .iter()
            .filter(|choice| choice.picked)
            .map(|choice| choice.target.clone())
            .collect();
        let installed = match crate::installed_plugins() {
            Ok(installed) => installed,
            Err(error) => {
                self.results = picked
                    .into_iter()
                    .map(|target| LaunchResult {
                        target,
                        message: format!("skipped: could not re-read installed plugins: {}", error),
                        success: false,
                    })
                    .collect();
                self.stage = Stage::Results;
                return;
            }
        };
        self.results = picked
            .into_iter()
            .map(|target| {
                if !starter::launch_allowed(&target, &source.specs, &installed) {
                    return LaunchResult {
                        target,
                        message: "skipped: no longer installed, enabled, matched, or declared"
                            .to_string(),
                        success: false,
                    };
                }
                let message = match target.kind {
                    LaunchKind::Action => {
                        crate::invoke_action(&target.plugin_id, &target.entrypoint)
                    }
                    LaunchKind::Pane => crate::open_pane(&target.plugin_id, &target.entrypoint),
                };
                let success = !message.trim_start().starts_with("could not");
                LaunchResult {
                    target,
                    message: one_line(&message),
                    success,
                }
            })
            .collect();
        self.stage = Stage::Results;
        self.cursor = 0;
        self.flash = None;
    }

    fn move_cursor(&mut self, down: bool) {
        let len = match self.stage {
            Stage::Review => self.plan.as_ref().map(|plan| plan.items.len()).unwrap_or(0),
            Stage::Launch => self.launch_choices.len(),
            Stage::Results => self.results.len(),
        };
        if down {
            self.cursor = (self.cursor + 1).min(len.saturating_sub(1));
        } else {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    fn draw(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        if self.pending_operation.is_some() {
            return self.draw_operation_confirm(out, width, height);
        }
        if self.pending_launch {
            return self.draw_launch_confirm(out, width, height);
        }
        match self.stage {
            Stage::Review => self.draw_review(out, width, height),
            Stage::Launch => self.draw_launch(out, width, height),
            Stage::Results => self.draw_results(out, width, height),
        }
    }

    fn draw_review(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        let rule = "─".repeat((width as usize).clamp(20, 200));
        write!(out, "\x1b[H\x1b[2J")?;
        writeln!(
            out,
            "\x1b[1m workspace starter\x1b[0m  \x1b[2m{}\x1b[0m\r",
            self.source_label()
        )?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;

        if let Some(error) = &self.error {
            writeln!(
                out,
                " \x1b[31mcannot read installed plugins:\x1b[0m {}\r",
                one_line(error)
            )?;
        }
        if self.selected_source == SourceKind::Profile && self.profile_source.is_none() {
            writeln!(out, " \x1b[31mworkspace profile is unavailable:\x1b[0m\r")?;
            writeln!(
                out,
                "   {}\r",
                self.profile_error
                    .as_deref()
                    .map(one_line)
                    .unwrap_or_else(|| "no valid profile was found".to_string())
            )?;
            writeln!(
                out,
                "\r no action is available until you switch to the global bundle.\r"
            )?;
        } else if let Some(source) = self.active_source() {
            writeln!(
                out,
                " source: {}\r",
                truncate(
                    &one_line(&source.list_path.display().to_string()),
                    width as usize
                )
            )?;
            writeln!(
                out,
                " lock:   {}\r",
                truncate(
                    &one_line(&source.lock_path.display().to_string()),
                    width as usize
                )
            )?;
            if let Some(plan) = &self.plan {
                let ready = plan
                    .items
                    .iter()
                    .filter(|item| matches!(item.state, ItemState::Ready))
                    .count();
                let install = plan
                    .items
                    .iter()
                    .filter(|item| item.state.needs_install())
                    .count();
                let updates = plan
                    .items
                    .iter()
                    .filter(|item| item.state.needs_update())
                    .count();
                let disabled = plan
                    .items
                    .iter()
                    .filter(|item| matches!(item.state, ItemState::Disabled))
                    .count();
                writeln!(
                    out,
                    "\r {} entries · {} ready · {} to install/repair · {} updates · {} disabled\r",
                    plan.items.len(),
                    ready,
                    install,
                    updates,
                    disabled
                )?;
                let visible = (height as usize).saturating_sub(10).max(1);
                let start = if self.cursor >= visible {
                    self.cursor - visible + 1
                } else {
                    0
                };
                for (index, item) in plan.items.iter().enumerate().skip(start).take(visible) {
                    let pointer = if index == self.cursor {
                        "\x1b[7m>\x1b[0m"
                    } else {
                        " "
                    };
                    writeln!(
                        out,
                        "{} \x1b[1m{}{}\x1b[0m {:<44} \x1b[2m{}\x1b[0m\r",
                        pointer,
                        item.state.colour(),
                        item.state.marker(),
                        truncate(&item.spec.display(), 44),
                        truncate(
                            &one_line(&item.state.label()),
                            width.saturating_sub(52) as usize
                        )
                    )?;
                }
                if plan.items.len() > visible {
                    writeln!(out, "   \x1b[2m… use j/k to review all entries\x1b[0m\r")?;
                }
                writeln!(
                    out,
                    "\r \x1b[2mPreview only. No prune and no global-list edits; install and update are confirmed separately.\x1b[0m\r"
                )?;
            }
        }

        let footer = match &self.flash {
            Some(message) => format!("\x1b[36m{}\x1b[0m", one_line(message)),
            None if self.selected_source == SourceKind::Profile && self.profile_source.is_none() => {
                "\x1b[1m[b]\x1b[0m use global bundle explicitly  \x1b[1m[r]\x1b[0m re-read  \x1b[1m[q]\x1b[0m quit".to_string()
            }
            None => {
                "\x1b[1m[i]\x1b[0m install/repair  \x1b[1m[u]\x1b[0m update  \x1b[1m[l]\x1b[0m choose panes/actions  \x1b[1m[b]\x1b[0m switch  \x1b[1m[r]\x1b[0m re-read  \x1b[1m[q]\x1b[0m quit".to_string()
            }
        };
        self.footer(out, height, &rule, &footer)
    }

    fn draw_operation_confirm(
        &self,
        out: &mut impl Write,
        width: u16,
        height: u16,
    ) -> io::Result<()> {
        let rule = "─".repeat((width as usize).clamp(20, 200));
        let operation = self.pending_operation.expect("checked by caller");
        write!(out, "\x1b[H\x1b[2J")?;
        writeln!(
            out,
            "\x1b[1m workspace starter\x1b[0m  \x1b[33mconfirm {}\x1b[0m\r",
            operation_label(operation)
        )?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;
        if let Some(source) = self.active_source() {
            writeln!(
                out,
                " source: {}\r",
                one_line(&source.list_path.display().to_string())
            )?;
            writeln!(
                out,
                " lock:   {}\r",
                one_line(&source.lock_path.display().to_string())
            )?;
        }
        writeln!(out, "\r The preview found:\r")?;
        let lines: Vec<String> = self
            .plan
            .as_ref()
            .map(|plan| {
                plan.items
                    .iter()
                    .filter(|item| match operation {
                        Operation::Install => item.state.needs_install(),
                        Operation::Update => item.state.needs_update(),
                    })
                    .map(|item| format!("  • {} — {}", item.spec.display(), item.state.label()))
                    .collect()
            })
            .unwrap_or_default();
        let visible = (height as usize).saturating_sub(10).max(1);
        for line in lines.iter().take(visible) {
            writeln!(
                out,
                "{}\r",
                truncate(&one_line(line), width.saturating_sub(1) as usize)
            )?;
        }
        if lines.len() > visible {
            writeln!(out, "  … {} more\r", lines.len() - visible)?;
        }
        match operation {
            Operation::Install => {
                writeln!(
                    out,
                    "\r This installs missing entries and repairs pinned drift.\r"
                )?;
                writeln!(
                    out,
                    " It never prunes and writes only the selected source's lockfile.\r"
                )?;
            }
            Operation::Update => {
                writeln!(
                    out,
                    "\r This re-resolves only unpinned entries marked by the cached marketplace.\r"
                )?;
                writeln!(
                    out,
                    " It never changes the profile/global list or installs unrelated entries.\r"
                )?;
            }
        }
        self.footer(
            out,
            height,
            &rule,
            "\x1b[1m[y]\x1b[0m apply  \x1b[1m[n / esc]\x1b[0m cancel",
        )
    }

    fn draw_launch(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        let rule = "─".repeat((width as usize).clamp(20, 200));
        let picked = self
            .launch_choices
            .iter()
            .filter(|choice| choice.picked)
            .count();
        write!(out, "\x1b[H\x1b[2J")?;
        writeln!(
            out,
            "\x1b[1m workspace starter\x1b[0m  \x1b[2mchoose installed manifest targets · {} selected\x1b[0m\r",
            picked
        )?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;
        writeln!(
            out,
            " Only actions and panes declared by installed plugins appear here.\r"
        )?;
        let visible = (height as usize).saturating_sub(7).max(1);
        let start = if self.cursor >= visible {
            self.cursor - visible + 1
        } else {
            0
        };
        for (index, choice) in self
            .launch_choices
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
        {
            let pointer = if index == self.cursor {
                "\x1b[7m>\x1b[0m"
            } else {
                " "
            };
            let tick = if choice.picked {
                "\x1b[32m[x]\x1b[0m"
            } else {
                "[ ]"
            };
            writeln!(
                out,
                "{} {} {}\r",
                pointer,
                tick,
                truncate(
                    &target_label(&choice.target),
                    width.saturating_sub(8) as usize
                )
            )?;
        }
        self.footer(
            out,
            height,
            &rule,
            "\x1b[1m[space]\x1b[0m select  \x1b[1m[enter]\x1b[0m review launch  \x1b[1m[esc]\x1b[0m back",
        )
    }

    fn draw_launch_confirm(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        let rule = "─".repeat((width as usize).clamp(20, 200));
        write!(out, "\x1b[H\x1b[2J")?;
        writeln!(
            out,
            "\x1b[1m workspace starter\x1b[0m  \x1b[33mconfirm launch\x1b[0m\r"
        )?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;
        writeln!(
            out,
            " The following installed manifest targets will be requested:\r"
        )?;
        let selected: Vec<String> = self
            .launch_choices
            .iter()
            .filter(|choice| choice.picked)
            .map(|choice| format!("  • {}", target_label(&choice.target)))
            .collect();
        let visible = (height as usize).saturating_sub(8).max(1);
        for line in selected.iter().take(visible) {
            writeln!(
                out,
                "{}\r",
                truncate(&one_line(line), width.saturating_sub(1) as usize)
            )?;
        }
        writeln!(
            out,
            "\r Herdr will re-check that each target is still installed, enabled, and declared.\r"
        )?;
        writeln!(
            out,
            " Unavailable targets are skipped; remaining safe targets continue.\r"
        )?;
        self.footer(
            out,
            height,
            &rule,
            "\x1b[1m[y]\x1b[0m launch  \x1b[1m[n / esc]\x1b[0m cancel",
        )
    }

    fn draw_results(&self, out: &mut impl Write, width: u16, height: u16) -> io::Result<()> {
        let rule = "─".repeat((width as usize).clamp(20, 200));
        write!(out, "\x1b[H\x1b[2J")?;
        writeln!(
            out,
            "\x1b[1m workspace starter\x1b[0m  \x1b[2mresults\x1b[0m\r"
        )?;
        writeln!(out, "\x1b[2m{}\x1b[0m\r", rule)?;
        if self.results.is_empty() {
            writeln!(out, " nothing was launched.\r")?;
        } else {
            let visible = (height as usize).saturating_sub(5).max(1);
            for result in self.results.iter().take(visible) {
                let colour = if result.success {
                    "\x1b[32m"
                } else {
                    "\x1b[31m"
                };
                writeln!(
                    out,
                    " {}{}\x1b[0m {} — {}\r",
                    colour,
                    if result.success { "✔" } else { "✗" },
                    truncate(&target_label(&result.target), 42),
                    truncate(
                        &one_line(&result.message),
                        width.saturating_sub(52) as usize
                    )
                )?;
            }
            if self.results.len() > visible {
                writeln!(out, " … {} more results\r", self.results.len() - visible)?;
            }
        }
        writeln!(
            out,
            "\r Partial failures are shown above; successful targets were not rolled back.\r"
        )?;
        self.footer(out, height, &rule, "\x1b[1m[esc/q]\x1b[0m close")
    }

    fn footer(
        &self,
        out: &mut impl Write,
        height: u16,
        rule: &str,
        footer: &str,
    ) -> io::Result<()> {
        let footer = match &self.flash {
            Some(message) => format!("\x1b[36m{}\x1b[0m", one_line(message)),
            None => footer.to_string(),
        };
        write!(
            out,
            "\x1b[{};1H\x1b[2m{}\r\n \x1b[0m{}\r",
            height.saturating_sub(1),
            rule,
            footer
        )?;
        out.flush()
    }
}

fn operation_label(operation: Operation) -> &'static str {
    match operation {
        Operation::Install => "install/repair",
        Operation::Update => "update",
    }
}

fn target_label(target: &LaunchTarget) -> String {
    let kind = match target.kind {
        LaunchKind::Action => "action",
        LaunchKind::Pane => "pane",
    };
    format!(
        "{} · {} ({}, {})",
        one_line(&target.plugin_id),
        one_line(&target.title),
        kind,
        one_line(&target.entrypoint)
    )
}

fn one_line(input: &str) -> String {
    input
        .chars()
        .map(|character| {
            if character.is_control() {
                '�'
            } else {
                character
            }
        })
        .collect()
}

fn truncate(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        return input.to_string();
    }
    let keep: String = input.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", keep)
}

fn event_loop(out: &mut impl Write) -> io::Result<()> {
    let mut app = App::load();
    loop {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        app.draw(out, width, height)?;
        let key = match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => key,
            Event::Resize(..) | Event::Mouse(_) => continue,
            _ => continue,
        };

        if app.pending_operation.is_some() {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                app.commit_operation()?;
            } else {
                app.pending_operation = None;
                app.flash = Some("starter operation cancelled".to_string());
            }
            continue;
        }
        if app.pending_launch {
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                app.commit_launch();
            } else {
                app.pending_launch = false;
                app.flash = Some("launch cancelled".to_string());
            }
            continue;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if key.code == KeyCode::Char('c') {
                return Ok(());
            }
            continue;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            continue;
        }
        app.flash = None;

        match app.stage {
            Stage::Review => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('j') | KeyCode::Down => app.move_cursor(true),
                KeyCode::Char('k') | KeyCode::Up => app.move_cursor(false),
                KeyCode::Char('g') | KeyCode::Home => app.cursor = 0,
                KeyCode::Char('G') | KeyCode::End => {
                    app.cursor = app
                        .plan
                        .as_ref()
                        .map(|plan| plan.items.len())
                        .unwrap_or(0)
                        .saturating_sub(1)
                }
                KeyCode::Char('i') => app.request_operation(Operation::Install),
                KeyCode::Char('u') => app.request_operation(Operation::Update),
                KeyCode::Char('l') => app.open_launch(),
                KeyCode::Char('b') => app.toggle_source(),
                KeyCode::Char('r') => app.refresh(),
                _ => {}
            },
            Stage::Launch => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    app.stage = Stage::Review;
                    app.cursor = 0;
                }
                KeyCode::Char('j') | KeyCode::Down => app.move_cursor(true),
                KeyCode::Char('k') | KeyCode::Up => app.move_cursor(false),
                KeyCode::Char('g') | KeyCode::Home => app.cursor = 0,
                KeyCode::Char('G') | KeyCode::End => {
                    app.cursor = app.launch_choices.len().saturating_sub(1)
                }
                KeyCode::Char(' ') => {
                    if let Some(choice) = app.launch_choices.get_mut(app.cursor) {
                        choice.picked = !choice.picked;
                    }
                }
                KeyCode::Enter => app.request_launch(),
                _ => {}
            },
            Stage::Results => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                _ => {}
            },
        }
    }
}

pub(crate) fn run() -> io::Result<()> {
    let mut out = io::stdout();
    if let Err(error) = terminal::enable_raw_mode() {
        eprintln!(
            "herdr-lazy starter needs a terminal, and does not have one ({}).",
            error
        );
        eprintln!();
        eprintln!("If you ran this as a herdr plugin action or keybinding: actions get no PTY.");
        eprintln!("Open the pane instead:");
        eprintln!("  {}", crate::starter_pane_hint());
        return Ok(());
    }
    write!(out, "\x1b[?1049h\x1b[?25l")?;
    out.flush()?;
    let result = event_loop(&mut out);
    write!(out, "\x1b[?25h\x1b[?1049l")?;
    out.flush()?;
    terminal::disable_raw_mode()?;
    result
}
