//! The AI cursor: a second, visibly different pointer that shows where an agent is working.
//!
//! Agents drive FilmCraft through the control channel (`ui.click`, `ui.drag`, `engine.execute`…).
//! Their pointer input reaches the UI one event per frame; this overlay follows it, glides to each
//! point and pulses where it clicks, with a small chip naming the element or command. Engine
//! commands with no pointer (`timeline.move {clip}`) fly to the clip or item they act on. The
//! overlay never takes input, and it fades out when the agent has been idle for a while.
//!
//! While the cursor is still gliding the next pointer step is held (see [`AiCursor::holding`]), so
//! the user sees it arrive before the click lands. The hold is capped, so a stuck cursor can never
//! stall the agent.

use egui::{Color32, Context, FontId, LayerId, Order, Pos2, Shape, Stroke, vec2};

/// The agent's colour: the same orange as the Assistant panel's header.
const ACCENT: Color32 = Color32::from_rgb(0xd9, 0x8b, 0x5f);
/// Seconds the cursor stays fully visible after the last action, and how long it then fades.
const LINGER: f64 = 2.5;
const FADE: f64 = 1.0;
/// Seconds a click ripple lasts.
const RIPPLE: f64 = 0.55;
/// A pointer step waits while the cursor is further than this from where the step is.
const ARRIVED: f32 = 10.0;
/// Most frames a step is held for (about 0.6 s at 60 fps).
const MAX_HOLD_FRAMES: u32 = 36;
/// Higher glides faster: the share of the remaining distance covered per second, in e-folds.
const GLIDE_RATE: f32 = 20.0;

pub struct AiCursor {
    /// Shown at all. Off: nothing is drawn and nothing is held back.
    pub enabled: bool,
    pos: Pos2,
    target: Pos2,
    placed: bool,
    pressed: bool,
    label: String,
    /// An action arrived since the last paint; stamped with the paint time there.
    touched: bool,
    last_active: f64,
    /// Ripples waiting for a paint-time stamp, then the live ones as `(centre, start)`.
    new_ripples: Vec<Pos2>,
    ripples: Vec<(Pos2, f64)>,
    held_frames: u32,
}

impl Default for AiCursor {
    fn default() -> Self {
        AiCursor {
            enabled: true,
            pos: Pos2::ZERO,
            target: Pos2::ZERO,
            placed: false,
            pressed: false,
            label: String::new(),
            touched: false,
            last_active: f64::NEG_INFINITY,
            new_ripples: Vec::new(),
            ripples: Vec::new(),
            held_frames: 0,
        }
    }
}

impl AiCursor {
    /// Follow the pointer events an agent's input step is about to deliver.
    pub fn note_events(&mut self, events: &[egui::Event]) {
        for event in events {
            match event {
                egui::Event::PointerMoved(p) => self.move_to(*p),
                egui::Event::PointerButton { pos, pressed, .. } => {
                    self.move_to(*pos);
                    self.pressed = *pressed;
                    if *pressed {
                        self.new_ripples.push(*pos);
                    }
                }
                egui::Event::MouseWheel { .. } => self.touched = true,
                egui::Event::Key { .. } | egui::Event::Text(_) => self.touched = true,
                _ => {}
            }
        }
    }

    /// Fly to `pos`, naming what the agent is doing there.
    pub fn glide_to(&mut self, pos: Pos2, label: &str) {
        self.move_to(pos);
        self.label = label.to_string();
        self.new_ripples.push(pos);
    }

    /// Name what the agent is doing, where the cursor is.
    pub fn set_label(&mut self, label: &str) {
        self.label = label.to_string();
        self.touched = true;
    }

    fn move_to(&mut self, pos: Pos2) {
        if !pos.x.is_finite() || !pos.y.is_finite() {
            return;
        }
        // A press outside the window is how scrolling resets the pointer; don't fly there.
        if pos.x < 0.0 || pos.y < 0.0 {
            return;
        }
        if !self.placed {
            self.pos = pos;
            self.placed = true;
        }
        self.target = pos;
        self.touched = true;
    }

    /// Whether the next pointer step should wait for the cursor to arrive. Counts the frames it
    /// has waited, and stops waiting after [`MAX_HOLD_FRAMES`].
    pub fn holding(&mut self) -> bool {
        let far = self.enabled && self.placed && (self.pos - self.target).length() > ARRIVED;
        if far && self.held_frames < MAX_HOLD_FRAMES {
            self.held_frames += 1;
            true
        } else {
            self.held_frames = 0;
            false
        }
    }

    /// Draw the cursor above everything else, and keep the UI repainting while it moves or fades.
    pub fn paint(&mut self, ctx: &Context) {
        if !self.enabled || !self.placed {
            return;
        }
        let now = ctx.input(|i| i.time);
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        if std::mem::take(&mut self.touched) {
            self.last_active = now;
        }
        for centre in self.new_ripples.drain(..) {
            self.ripples.push((centre, now));
        }
        self.ripples.retain(|(_, start)| now - start < RIPPLE);

        let remaining = self.target - self.pos;
        if remaining.length() > 0.4 {
            self.pos += remaining * (1.0 - (-GLIDE_RATE * dt).exp());
            ctx.request_repaint();
        } else {
            self.pos = self.target;
        }

        let idle = now - self.last_active;
        let visibility = if idle < LINGER { 1.0 } else { (1.0 - (idle - LINGER) / FADE).clamp(0.0, 1.0) } as f32;
        if visibility <= 0.0 && self.ripples.is_empty() {
            return;
        }
        if !self.ripples.is_empty() || visibility < 1.0 {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((LINGER - idle).max(0.05)));
        }

        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, egui::Id::unique("ai-cursor")));
        let fade = |c: Color32, a: f32| c.gamma_multiply(a.clamp(0.0, 1.0));

        for (centre, start) in &self.ripples {
            let t = ((now - start) / RIPPLE) as f32;
            let ease = 1.0 - (1.0 - t).powi(3);
            painter.circle_stroke(*centre, 6.0 + 26.0 * ease, Stroke::new(2.0 * (1.0 - t) + 0.5, fade(ACCENT, 1.0 - t)));
        }
        if visibility <= 0.0 {
            return;
        }

        // Soft glow, then a pointer with a dark outline so it reads on any background.
        painter.circle_filled(self.pos + vec2(3.0, 5.0), 17.0, fade(ACCENT, 0.16 * visibility));
        painter.circle_filled(self.pos + vec2(3.0, 5.0), 9.0, fade(ACCENT, 0.18 * visibility));
        let scale = if self.pressed { 0.88 } else { 1.0 };
        let arrow: Vec<Pos2> = [(0.0, 0.0), (0.0, 17.0), (4.4, 13.2), (7.2, 20.0), (10.2, 18.7), (7.5, 12.0), (13.0, 12.0)]
            .iter()
            .map(|(x, y)| self.pos + vec2(*x, *y) * scale)
            .collect();
        painter.add(Shape::convex_polygon(arrow.iter().map(|p| *p + vec2(1.2, 1.6)).collect(), fade(Color32::BLACK, 0.35 * visibility), Stroke::NONE));
        painter.add(Shape::convex_polygon(arrow, fade(ACCENT, visibility), Stroke::new(1.4, fade(Color32::WHITE, visibility))));

        // The name chip.
        let text = if self.label.is_empty() { "Claude".to_string() } else { format!("Claude · {}", self.label) };
        let font = FontId::proportional(11.5);
        let galley = painter.layout_no_wrap(text, font, fade(Color32::WHITE, visibility));
        let chip = egui::Rect::from_min_size(self.pos + vec2(15.0, 21.0), galley.size() + vec2(14.0, 8.0));
        // Keep the chip on screen when the cursor is near the right or bottom edge.
        let screen = ctx.content_rect();
        let shift = vec2((screen.max.x - 4.0 - chip.max.x).min(0.0), (screen.max.y - 4.0 - chip.max.y).min(0.0));
        let chip = chip.translate(shift);
        painter.rect_filled(chip, chip.height() / 2.0, fade(Color32::from_rgb(0x2a, 0x1b, 0x14), 0.92 * visibility));
        painter.rect_stroke(chip, chip.height() / 2.0, Stroke::new(1.0, fade(ACCENT, visibility)), egui::StrokeKind::Inside);
        painter.galley(chip.min + vec2(7.0, 4.0), galley, Color32::WHITE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_event_places_the_cursor_without_a_hold() {
        let mut c = AiCursor::default();
        c.note_events(&[egui::Event::PointerMoved(egui::pos2(300.0, 200.0))]);
        assert!(!c.holding(), "a cursor that starts at its target has nothing to wait for");
    }

    #[test]
    fn far_target_holds_the_next_step_but_never_forever() {
        let mut c = AiCursor::default();
        c.note_events(&[egui::Event::PointerMoved(egui::pos2(10.0, 10.0))]);
        c.note_events(&[egui::Event::PointerMoved(egui::pos2(900.0, 600.0))]);
        let held = (0..200).take_while(|_| c.holding()).count();
        assert_eq!(held as u32, MAX_HOLD_FRAMES);
    }

    #[test]
    fn disabled_cursor_never_holds() {
        let mut c = AiCursor { enabled: false, ..AiCursor::default() };
        c.note_events(&[egui::Event::PointerMoved(egui::pos2(10.0, 10.0))]);
        c.note_events(&[egui::Event::PointerMoved(egui::pos2(900.0, 600.0))]);
        assert!(!c.holding());
    }

    #[test]
    fn non_finite_and_off_screen_points_are_ignored() {
        let mut c = AiCursor::default();
        c.note_events(&[egui::Event::PointerMoved(egui::pos2(f32::NAN, 5.0)), egui::Event::PointerMoved(egui::pos2(-100.0, -100.0))]);
        assert!(!c.placed);
    }
}
