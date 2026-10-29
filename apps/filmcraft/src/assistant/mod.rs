//! The Assistant panel's host: a terminal running Claude Code, started on the user's click and
//! wired to this app. Starting a session opens the control server on a free loopback port with a
//! fresh random credential, writes a private MCP config that points `filmcraft-cli mcp --bridge`
//! at it, and launches `claude` with that config. The credential reaches the agent only through
//! its environment (`FILMCRAFT_CONTROL_TOKEN`), never a file or a command line.

mod terminal;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use egui::{Align2, Color32, FontId, Rect, RichText, Sense, Stroke, vec2};
use filmcraft_ui_egui::{AssistantHost, ControlRequest};
use terminal::{Launch, Palette, Terminal};

/// The agent's standing instructions, appended to Claude Code's own.
const SYSTEM_PROMPT: &str = "You are running inside FilmCraft's Assistant sidebar, a video editor. \
The `filmcraft` MCP server controls the running app. Start with doc_inspect to see the project and \
timeline, prefer command_run / command_batch (every edit is undoable with edit.undo) over clicking, \
and use ui_screenshot or render_frame to check what you did. The user watches an AI cursor follow \
your actions, so say briefly what you are about to do. Ask before deleting media or exporting.";

/// Read-only tools that need no approval; anything that edits asks in the terminal first.
const READ_ONLY_TOOLS: &str = "mcp__filmcraft__doc_inspect mcp__filmcraft__project_inspect mcp__filmcraft__sequence_inspect \
mcp__filmcraft__command_list mcp__filmcraft__ui_inspect mcp__filmcraft__ui_elements mcp__filmcraft__ui_screenshot mcp__filmcraft__render_frame";

enum State {
    Idle,
    Failed(String),
    Running(Box<Terminal>),
}

pub struct Assistant {
    state: State,
    palette: Palette,
    /// Handed to the app once a session has started its control server.
    control: Option<Receiver<ControlRequest>>,
    /// Folder of the private MCP config; removed when the session ends.
    config_dir: Option<PathBuf>,
    working_dir: PathBuf,
    /// `FILMCRAFT_ASSISTANT_AUTOSTART` starts the session once, for unattended testing.
    autostarted: bool,
}

impl Assistant {
    pub fn new() -> Self {
        Assistant { state: State::Idle, palette: Palette::dark(), control: None, config_dir: None, working_dir: working_dir(), autostarted: false }
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.cleanup();
        self.state = match self.try_start(ctx) {
            Ok(terminal) => State::Running(Box::new(terminal)),
            Err(e) => State::Failed(e),
        };
    }

    fn try_start(&mut self, ctx: &egui::Context) -> Result<Terminal, String> {
        let claude = find_claude().ok_or("Claude Code was not found. Install it from claude.com/code, then try again.")?;
        let cli = find_cli().ok_or("filmcraft-cli was not found next to the app. Build it with `cargo build --release -p filmcraft-cli`.")?;
        // A credential the user already exported (as for `--control`) is reused, so their own tools
        // can talk to this session; otherwise a fresh one is made.
        let token = match std::env::var("FILMCRAFT_CONTROL_TOKEN") {
            Ok(t) if t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()) => t,
            _ => random_token()?,
        };
        let (rx, port) = crate::control_server::start_ephemeral(ctx.clone(), token.clone())?;
        self.control = Some(rx);
        log::info!("assistant: control server on 127.0.0.1:{port}");

        let dir = private_dir()?;
        let config = dir.join("mcp.json");
        // `${FILMCRAFT_CONTROL_TOKEN}` is expanded by Claude Code from the environment below, so
        // the credential itself is never written to disk.
        let json = serde_json::json!({"mcpServers": {"filmcraft": {
            "command": cli,
            "args": ["mcp", "--bridge", format!("127.0.0.1:{port}")],
            "env": {"FILMCRAFT_CONTROL_TOKEN": "${FILMCRAFT_CONTROL_TOKEN}"},
        }}});
        std::fs::write(&config, json.to_string()).map_err(|e| format!("could not write the MCP config: {e}"))?;
        self.config_dir = Some(dir);

        let mut env = HashMap::new();
        env.insert("FILMCRAFT_CONTROL_TOKEN".to_string(), token);
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        let args = vec![
            "--mcp-config".into(),
            config.to_string_lossy().into_owned(),
            "--append-system-prompt".into(),
            SYSTEM_PROMPT.into(),
            "--allowedTools".into(),
            READ_ONLY_TOOLS.into(),
        ];
        Terminal::spawn(Launch { program: claude, args, env, cwd: Some(self.working_dir.clone()) }, ctx)
    }

    fn cleanup(&mut self) {
        if let Some(dir) = self.config_dir.take() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Drop for Assistant {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl AssistantHost for Assistant {
    fn show(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        if matches!(self.state, State::Idle) && !self.autostarted && std::env::var_os("FILMCRAFT_ASSISTANT_AUTOSTART").is_some() {
            self.autostarted = true;
            self.start(&ctx);
        }
        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 34.0));
        let body = Rect::from_min_max(bar.left_bottom(), rect.max);
        ui.painter().rect_filled(rect, 0.0, self.palette.background);

        // Header: a name, what the session is doing, and a restart button.
        let (status, dot) = match &self.state {
            State::Idle => ("not started", Color32::from_gray(110)),
            State::Failed(_) => ("could not start", Color32::from_rgb(0xee, 0x6b, 0x7b)),
            State::Running(t) if t.exited().is_some() => ("ended", Color32::from_gray(110)),
            State::Running(_) => ("connected to FilmCraft", Color32::from_rgb(0x8f, 0xd1, 0x8a)),
        };
        let painter = ui.painter();
        painter.rect_filled(bar, 0.0, Color32::from_rgb(0x15, 0x18, 0x1f));
        painter.line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, Color32::from_rgb(0x24, 0x28, 0x33)));
        painter.text(bar.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, "✦ Claude", FontId::proportional(13.0), Color32::from_rgb(0xd9, 0x8b, 0x5f));
        painter.circle_filled(bar.left_center() + vec2(92.0, 0.0), 3.5, dot);
        painter.text(bar.left_center() + vec2(103.0, 0.0), Align2::LEFT_CENTER, status, FontId::proportional(11.0), Color32::from_gray(150));
        let restart = Rect::from_center_size(bar.right_center() - vec2(30.0, 0.0), vec2(52.0, 20.0));
        let running = matches!(self.state, State::Running(_));
        if running {
            let r = ui.interact(restart, ui.make_persistent_id("assistant-restart"), Sense::click());
            let fill = if r.hovered() { Color32::from_rgb(0x2a, 0x2f, 0x3b) } else { Color32::from_rgb(0x1e, 0x22, 0x2b) };
            ui.painter().rect_filled(restart, 5.0, fill);
            ui.painter().text(restart.center(), Align2::CENTER_CENTER, "Restart", FontId::proportional(11.0), Color32::from_gray(190));
            if r.clicked() {
                self.start(&ctx);
                return;
            }
        }

        match &mut self.state {
            State::Running(terminal) => {
                let area = body.shrink2(vec2(10.0, 8.0));
                terminal.show(ui, area, ui.make_persistent_id("assistant-terminal"), &self.palette);
            }
            State::Idle | State::Failed(_) => {
                let message = match &self.state {
                    State::Failed(e) => Some(e.clone()),
                    _ => None,
                };
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body.shrink(24.0)).layout(egui::Layout::top_down(egui::Align::Center)));
                child.add_space((body.height() * 0.28).min(160.0));
                child.label(RichText::new("✦").size(34.0).color(Color32::from_rgb(0xd9, 0x8b, 0x5f)));
                child.add_space(6.0);
                child.label(RichText::new("Edit with Claude").size(17.0).color(Color32::from_gray(225)));
                child.add_space(6.0);
                child.label(
                    RichText::new("Claude Code, connected to this project. It can read the timeline, make undoable edits and show you what it is doing.")
                        .size(12.0)
                        .color(Color32::from_gray(140)),
                );
                child.add_space(16.0);
                if child
                    .add(
                        egui::Button::new(RichText::new("Start session").size(13.0).color(Color32::WHITE))
                            .fill(Color32::from_rgb(0xb9, 0x6a, 0x3f))
                            .corner_radius(6.0)
                            .min_size(vec2(150.0, 32.0)),
                    )
                    .clicked()
                {
                    self.start(&ctx);
                }
                if let Some(message) = message {
                    child.add_space(12.0);
                    child.label(RichText::new(message).size(11.5).color(Color32::from_rgb(0xee, 0x6b, 0x7b)));
                }
            }
        }
    }

    fn take_control(&mut self) -> Option<Receiver<ControlRequest>> {
        self.control.take()
    }
}

/// The folder Claude Code starts in: its own, inside FilmCraft's data folder, so the "trust this
/// folder" question is about a folder that holds nothing else (not the user's whole home).
fn working_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data = std::env::var_os("FILMCRAFT_DATA_DIR").map(PathBuf::from).or_else(|| {
        let home = home.clone()?;
        Some(if cfg!(target_os = "macos") { home.join("Library/Application Support/FilmCraft") } else { home.join(".local/share/FilmCraft") })
    });
    let dir = data.map(|d| d.join("assistant"));
    match dir {
        Some(dir) if std::fs::create_dir_all(&dir).is_ok() => dir,
        _ => home.unwrap_or_else(std::env::temp_dir),
    }
}

/// 32 random bytes as 64 hex characters.
fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| format!("could not make a control credential: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// A new folder only the user can enter, for the session's MCP config.
fn private_dir() -> Result<PathBuf, String> {
    let mut suffix = [0u8; 6];
    getrandom::fill(&mut suffix).map_err(|e| e.to_string())?;
    let name: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
    let dir = std::env::temp_dir().join(format!("filmcraft-assistant-{name}"));
    std::fs::create_dir(&dir).map_err(|e| format!("could not make a private folder: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

/// Where Claude Code may be installed. A desktop app launched from Finder has a short `PATH`, so
/// the usual install folders are tried as well.
fn find_claude() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect();
    if let Some(home) = &home {
        dirs.extend([home.join(".local/bin"), home.join(".claude/local"), home.join(".npm-global/bin"), home.join(".bun/bin")]);
    }
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    dirs.into_iter().map(|d| d.join("claude")).find(|p| is_executable(p))
}

/// `filmcraft-cli`: beside this executable (the packaged app, or `target/release`), else on `PATH`.
fn find_cli() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.join("filmcraft-cli")));
    beside
        .filter(|p| is_executable(p))
        .or_else(|| std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("filmcraft-cli")).find(|p| is_executable(p)))
}

fn is_executable(path: &Path) -> bool {
    path.is_file() && {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            path.metadata().is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            true
        }
    }
}
