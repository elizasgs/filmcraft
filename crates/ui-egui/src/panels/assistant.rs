//! Assistant panel: the host's terminal running an AI agent (Claude Code) that drives FilmCraft
//! through the control channel. Without a host (the web build) the panel only says so.

use egui::Rect;

use crate::FilmcraftApp;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let Some(host) = app.hooks.assistant.as_mut() else {
        let text = tl!("The assistant is available in the desktop app.");
        crate::dock::placeholder(ui, rect, &app.tokens, text);
        return;
    };
    host.show(ui, rect);
    if let Some(rx) = host.take_control()
        && app.control_rx.is_none()
    {
        app.control_rx = Some(rx);
    }
}
