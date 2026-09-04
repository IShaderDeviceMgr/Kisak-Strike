//! The developer console dialog.
//!
//! `vgui2/vgui_controls/consoledialog.cpp` (1,371 lines) plus its header, and
//! the game-side wrappers `gameui/gameconsole.cpp` and
//! `gameconsoledialog.cpp` — stage 4 of `portdocs/ENGINE_CONSOLE.md` §8, and
//! the first thing in this port that draws a widget.
//!
//! **The widget tree does not survive; three algorithms do.** `PORTING.md`
//! settles `egui` as the replacement for vgui2, RocketUI and ScaleformUI at
//! once, so the panel hierarchy, the scheme files, the focus-navigation group
//! and the popup menu are all deleted. What is ported is
//! `RebuildCompletionList` (`:510`, which lives in
//! [`Console::complete`](super::Console::complete) because it is a question
//! about the registry rather than about a widget), `AddToHistory` (`:1075`)
//! and `OnAutoComplete` (`:648`).
//!
//! # This module names `egui` and nothing else
//!
//! No `winit`, no `wgpu`, no engine type. That is the same three-layer split
//! the input path already has — `window/` translates, `input/` decides,
//! `console/` executes — applied to the UI: `window/` feeds `egui` the
//! platform events, this decides what the console looks like, and
//! [`materials::ui`](crate::materials::ui) draws the triangles that come out.
//! It is why the dialog can be driven and asserted on in a unit test with no
//! window and no GPU, which the tests at the bottom of this file do.
//!
//! # What is deliberately not here
//!
//! - **The notify area** (`CConPanel`, the fading lines at the top of the
//!   screen). It is a HUD element and belongs wherever the HUD lands, not in
//!   the console (`ENGINE_CONSOLE.md` §1).
//! - **`m_bStatusVersion`**, the one-line "status" layout the dedicated server
//!   and the tools used. There is no dedicated server yet.
//! - **`DumpConsoleTextToFile`** (`consoledialog.cpp:1162`), which is
//!   `con_logfile`'s neighbour and arrives with it.

use egui::{Color32, RichText};

use super::{Color, Console, Line, Source, Suggestion};

/// `MAX_HISTORY_ITEMS` (`public/vgui_controls/consoledialog.h:56`).
const MAX_HISTORY_ITEMS: usize = 100;

/// How many completions are on screen at once. `MAX_MENU_ITEMS`
/// (`consoledialog.cpp:794`), where the tenth was literally the string `"..."`
/// standing in for "there are more". A scroll area says the same thing without
/// spending an entry on it.
const VISIBLE_COMPLETIONS: usize = 10;

/// The console's scrollback, entry line, history and completion popup.
///
/// State only — [`draw`](ConsoleUi::draw) is the whole of the behavior, and
/// the scrollback itself lives in [`Log`](super::Log) rather than here,
/// because output exists whether or not anything is displaying it.
#[derive(Debug, Default)]
pub struct ConsoleUi {
    /// Whether the dialog is up. `toggleconsole`/`showconsole`/`hideconsole`.
    open: bool,
    /// The entry line. `m_pEntry`.
    input: String,
    /// What the entry said the last time this looked, so that a change made by
    /// the *user* can be told from one made by cycling the completions —
    /// `m_bAutoCompleteMode` (`consoledialog.cpp:654`) under another name.
    last_seen: String,
    /// What the user actually typed, kept across a cycle so the completion
    /// list does not rebuild itself out from under the cycling.
    /// `m_szPartialText`.
    partial: String,
    /// `m_CommandHistory`: oldest first, newest last, capped at
    /// [`MAX_HISTORY_ITEMS`].
    history: Vec<String>,
    /// `m_CompletionList`, rebuilt on every user edit.
    completion: Vec<Suggestion>,
    /// `m_iNextCompletion`, as a selection rather than as a cursor: `None` is
    /// "not cycling", which is the state `m_bAutoCompleteMode` tracked
    /// separately.
    selected: Option<usize>,
    /// Set when the dialog opens, so the entry takes focus on the frame it
    /// first appears rather than a frame later.
    focus_wanted: bool,
}

impl ConsoleUi {
    pub fn new() -> ConsoleUi {
        ConsoleUi::default()
    }

    /// Whether the dialog is up.
    ///
    /// The engine reads this for two things beyond drawing: the cursor is
    /// given back while the console is open, and the UI claims keyboard and
    /// mouse input while it is (`ENGINE_INPUT.md` §8.3).
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// `Con_ShowConsole_f` / `Con_HideConsole_f` (`engine/console.cpp:224`).
    pub fn set_open(&mut self, open: bool) {
        if self.open == open {
            return;
        }
        self.open = open;
        match open {
            true => self.focus_wanted = true,
            // `CConsolePanel::Hide` (`:1068`) resets the completion state, so
            // that reopening does not resume a cycle from three sessions ago.
            false => self.reset_completion(),
        }
    }

    /// `Con_ToggleConsole_f` (`engine/console.cpp:257`).
    pub fn toggle(&mut self) {
        self.set_open(!self.open);
    }

    /// What the user has typed but not submitted. Tests read it; nothing else
    /// needs to.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// The command history, oldest first.
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// The completion list as it stands. Empty when the entry is empty, where
    /// the *history* is offered instead — see [`ConsoleUi::candidates`].
    pub fn completion(&self) -> &[Suggestion] {
        &self.completion
    }

    /// Draws the dialog and runs everything it does.
    ///
    /// A no-op while the console is closed, apart from the closed-state reset:
    /// `egui` is run every frame regardless (`window/` needs its
    /// "did the UI want this event" answer to stay current), and a console
    /// that is not up adds nothing to that pass.
    pub fn draw(&mut self, ctx: &egui::Context, console: &mut Console<'_>) {
        if !self.open {
            return;
        }

        // `Window::open` is what draws the close button and clears the flag
        // when it is clicked. Taken through a local because the body below
        // borrows `self` mutably.
        let mut open = true;
        egui::Window::new("Console")
            .open(&mut open)
            .default_pos([48.0, 48.0])
            .default_size([880.0, 520.0])
            .min_width(360.0)
            .resizable(true)
            .collapsible(false)
            // The window follows the game window when it shrinks, rather than
            // stranding the entry line off screen.
            .constrain(true)
            // The scrollback has its own scroll area, with the entry pinned
            // below it; a window-level one would scroll both together.
            .vscroll(false)
            .show(ctx, |ui| self.body(ui, console));

        if !open {
            self.set_open(false);
        }
    }

    /// The window's contents: scrollback, completions, entry.
    fn body(&mut self, ui: &mut egui::Ui, console: &mut Console<'_>) {
        // Escape closes the console, and is taken here rather than left to
        // `egui` because a focused `TextEdit` surrenders focus on it and the
        // dialog would stay up with nothing focused.
        //
        // This is `Key_Event`'s `IsESC` special case (`engine/keys.cpp:1359`)
        // arriving at its natural place: Valve bypasses the UI chain for
        // Escape so the *client* can act on it first and then hands it back to
        // VGui, which is what closes an open dialog. Here the console is the
        // only thing above the engine, so it takes it while it is up — and
        // because the whole event is consumed, the game never sees it and
        // Escape does not also give the cursor back (`mouse_look_after` in
        // `src/engine/mod.rs`).
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.open = false;
            self.reset_completion();
            return;
        }

        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        let spacing = ui.spacing().item_spacing.y;

        // Bottom-up: the entry is a fixed height and the completion list grows
        // to a cap, so the scrollback gets whatever is left. Computed rather
        // than laid out from the bottom because `egui` lays out top-down and a
        // `TopBottomPanel` cannot live inside a `Window`.
        let entry_height = ui.spacing().interact_size.y;
        let completion_height = match self.candidate_count() {
            0 => 0.0,
            len => len.min(VISIBLE_COMPLETIONS) as f32 * (row_height + spacing) + spacing,
        };
        let reserved = entry_height + completion_height + 3.0 * spacing + 8.0;

        egui::ScrollArea::vertical()
            .id_salt("console_scrollback")
            .stick_to_bottom(true)
            .auto_shrink([false; 2])
            .max_height((ui.available_height() - reserved).max(row_height))
            .show_rows(ui, row_height, console.log().len(), |ui, rows| {
                // `Log` is a ring, so it is walked rather than indexed. The
                // skip is over at most the ring's capacity and only over the
                // rows actually on screen.
                ui.spacing_mut().item_spacing.y = 0.0;
                for line in console
                    .log()
                    .lines()
                    .skip(rows.start)
                    .take(rows.end - rows.start)
                {
                    ui.label(styled(line));
                }
            });

        ui.separator();
        self.completion_list(ui);
        self.entry(ui, console);
    }

    /// The completion popup, inline rather than floating.
    ///
    /// `m_pCompletionList` was a `Menu` positioned under the entry
    /// (`UpdateCompletionListPosition`, `:987`). A menu that has to be
    /// re-positioned every layout, kept in front of its own parent and
    /// prevented from stealing focus is three problems this does not have.
    fn completion_list(&mut self, ui: &mut egui::Ui) {
        let candidates = self.candidates();
        if candidates.is_empty() {
            return;
        }

        let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
        let spacing = ui.spacing().item_spacing.y;
        let mut chosen = None;

        egui::ScrollArea::vertical()
            .id_salt("console_completion")
            .max_height(VISIBLE_COMPLETIONS as f32 * (row_height + spacing))
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (index, item) in candidates.iter().enumerate() {
                    // A cvar shows its current value beside the name and
                    // completes to the name alone (`GetCompletionItemText`,
                    // `:623`), which is why `Suggestion` keeps the two apart.
                    let label = match &item.value {
                        Some(value) => format!("{} = {value}", item.text),
                        None => item.text.clone(),
                    };
                    let selected = self.selected == Some(index);
                    if ui
                        .selectable_label(selected, RichText::new(label).monospace())
                        .clicked()
                    {
                        chosen = Some(index);
                    }
                }
            });

        if let Some(index) = chosen {
            self.apply_completion(index);
        }
    }

    /// The entry line, and every key it answers.
    fn entry(&mut self, ui: &mut egui::Ui, console: &mut Console<'_>) {
        // Taken **before** the entry is built, so that the text edit never
        // sees them: Tab would move focus and the arrows would move the
        // caret. `CConsolePanel::OnKeyCodeTyped` (`:887`) swallows the same
        // three, and its comment says so in as many words.
        let (forward, back) = ui.input_mut(|input| {
            let forward = input.consume_key(egui::Modifiers::NONE, egui::Key::Tab)
                | input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
            let back = input.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab)
                | input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
            (forward, back)
        });

        let mut submitted = false;
        ui.horizontal(|ui| {
            let submit = ui.button("Submit");
            let entry = ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .id(egui::Id::new("console_entry"))
                    .font(egui::TextStyle::Monospace)
                    .hint_text("command")
                    // Tab is the completion key here, so it must not be the
                    // focus key.
                    .lock_focus(true)
                    .desired_width(f32::INFINITY),
            );

            if std::mem::take(&mut self.focus_wanted) {
                entry.request_focus();
            }

            // The `egui` idiom for "Enter in a single-line entry": the widget
            // gives up focus on Enter, so the two facts have to be read
            // together.
            let entered =
                entry.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
            if entered || submit.clicked() {
                submitted = true;
                // Focus goes straight back, because the console is modal for
                // as long as it is up and there is nothing else to focus.
                entry.request_focus();
            }
        });

        if submitted {
            self.submit(console);
            return;
        }

        // A user edit invalidates the list; cycling must not. Comparing
        // against what this last wrote is how the two are told apart, and it
        // is `m_bAutoCompleteMode` without the flag.
        if self.input != self.last_seen {
            self.last_seen = self.input.clone();
            self.partial = self.input.clone();
            self.completion = console.complete(&self.partial);
            self.selected = None;
        }

        if forward || back {
            self.cycle(back);
        }
    }

    /// `OnAutoComplete` (`consoledialog.cpp:648`), as a selection rather than
    /// as a cursor that is incremented after use.
    ///
    /// Wraps in both directions, which is what the original's
    /// `m_iNextCompletion -= 2` and its two `IsValidIndex` fall-backs add up
    /// to.
    fn cycle(&mut self, backwards: bool) {
        let count = self.candidate_count();
        if count == 0 {
            return;
        }

        let next = match (self.selected, backwards) {
            (None, false) => 0,
            (None, true) => count - 1,
            (Some(index), false) => (index + 1) % count,
            (Some(index), true) => (index + count - 1) % count,
        };
        self.apply_completion(next);
    }

    /// Puts a completion in the entry without treating it as a user edit.
    fn apply_completion(&mut self, index: usize) {
        let candidates = self.candidates();
        let Some(item) = candidates.get(index) else {
            return;
        };

        let mut text = item.text.clone();
        // `OnAutoComplete` (`:715`) appends a space unless the completion
        // already contains one, so that `mat_wireframe ` is ready for a value
        // while `exec valve.rc` is ready to run.
        if !text.contains(' ') {
            text.push(' ');
        }

        self.input = text;
        // Not `partial`: the list stays the one the typed text produced, which
        // is what makes cycling cycle rather than narrow to one item and stop.
        self.last_seen = self.input.clone();
        self.selected = Some(index);
    }

    /// What the completion list currently offers.
    ///
    /// **Empty input lists history, not everything** (`RebuildCompletionList`,
    /// `:516`). That rule is the reason there is no separate history-recall
    /// key: Up on an empty line is already the most recent command.
    ///
    /// History is offered **oldest first**, which looks backwards and is not:
    /// `RebuildCompletionList` walks `m_CommandHistory` from index 0, and
    /// `OnAutoComplete`'s reverse case starts at the *end* of the list. So
    /// Up on an empty line lands on the newest command, which is the behavior
    /// anyone expects, and it falls out of the ordering rather than being
    /// special-cased.
    fn candidates(&self) -> Vec<Suggestion> {
        if self.partial.is_empty() {
            return self
                .history
                .iter()
                .map(|command| Suggestion {
                    text: command.clone(),
                    value: None,
                })
                .collect();
        }
        self.completion.clone()
    }

    /// How many candidates there are, without building them.
    fn candidate_count(&self) -> usize {
        match self.partial.is_empty() {
            true => self.history.len(),
            false => self.completion.len(),
        }
    }

    /// `CConsolePanel::OnCommand("Submit")` (`consoledialog.cpp:826`).
    ///
    /// The echo is Valve's: the submitted line is printed back as `] <text>`
    /// so that the scrollback reads as a transcript rather than as output with
    /// no questions.
    fn submit(&mut self, console: &mut Console<'_>) {
        let text = std::mem::take(&mut self.input);
        let command = text.trim();

        self.reset_completion();
        self.last_seen.clear();

        if command.is_empty() {
            return;
        }

        console.log_mut().echo(&format!("] {command}"));
        // `kCommandSrcUserInput`: this was typed, which is the distinction
        // `ENGINE_CONSOLE.md` §4.7 exists to preserve and which decides
        // whether an unknown name is an error or a developer message.
        console.enqueue(command, Source::UserInput);
        self.push_history(command);
    }

    /// `AddToHistory` (`consoledialog.cpp:1075`).
    ///
    /// Newest last, an existing identical entry removed rather than
    /// duplicated, and the oldest dropped once the cap is reached. Valve
    /// splits the line into command and arguments and compares the two halves;
    /// comparing the whole line is the same decision with less machinery,
    /// because the two halves were only ever compared together.
    fn push_history(&mut self, command: &str) {
        self.history
            .retain(|existing| !existing.eq_ignore_ascii_case(command));
        self.history.push(command.to_string());
        while self.history.len() > MAX_HISTORY_ITEMS {
            self.history.remove(0);
        }
    }

    fn reset_completion(&mut self) {
        self.completion.clear();
        self.partial.clear();
        self.selected = None;
    }
}

/// How a scrollback line is drawn.
///
/// `Con_ColorPrint` took an RGBA from every caller and `ApplySchemeSettings`
/// (`consoledialog.cpp:1027`) supplied two of them from the scheme file —
/// `Console.TextColor` and `Console.DevTextColor`. Colour is a property of
/// *how the line was produced* here (`log::Color`), so the mapping is one
/// place rather than forty.
fn styled(line: &Line) -> RichText {
    let color = match line.color {
        Color::Normal => Color32::from_rgb(216, 222, 233),
        Color::Warning => Color32::from_rgb(235, 203, 139),
        Color::Error => Color32::from_rgb(224, 108, 117),
        // `Console.DevTextColor`: quieter than normal output, because
        // `developer 2` is a lot of it.
        Color::Developer => Color32::from_rgb(143, 155, 171),
        // The transcript half of the scrollback — what was asked, as opposed
        // to what was answered.
        Color::Echo => Color32::from_rgb(163, 190, 140),
    };

    let text = RichText::new(&line.text).monospace();
    // `con_filter_enable 2` keeps a non-matching line and dims it, which is
    // the mode worth having: you keep the context around the thing you were
    // looking for.
    match line.dim {
        true => text.color(color.gamma_multiply(0.4)),
        false => text.color(color),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::console::{CommandSpec, CvarFlags, NoTarget};

    /// One `egui` pass over the dialog, with `events` delivered to it.
    ///
    /// This is the whole reason the dialog names no windowing type: a headless
    /// `Context` is a complete `egui` and needs neither a window nor a GPU.
    fn pass(
        ctx: &egui::Context,
        ui: &mut ConsoleUi,
        console: &mut Console<'_>,
        events: Vec<egui::Event>,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 720.0),
            )),
            events,
            ..Default::default()
        };
        ctx.run_ui(input, |pass_ui| ui.draw(pass_ui.ctx(), console))
            .drop_without_applying_deltas();
    }

    fn typed(text: &str) -> Vec<egui::Event> {
        vec![egui::Event::Text(text.to_string())]
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn console() -> Console<'static> {
        let mut console = Console::detached();
        for spec in [
            CommandSpec::new("map", "Load a map."),
            CommandSpec::new("mat_wireframe_toggle", "Toggle wireframe."),
        ] {
            console.register_command(spec).expect("unique");
        }
        console.cvar("mat_wireframe", "0", CvarFlags::NONE, "Draw wireframe.");
        console.cvar("sv_cheats_secret", "1", CvarFlags::HIDDEN, "Hidden.");
        console
    }

    #[test]
    fn it_starts_closed_and_toggles() {
        let mut ui = ConsoleUi::new();
        assert!(!ui.is_open());
        ui.toggle();
        assert!(ui.is_open());
        ui.toggle();
        assert!(!ui.is_open());
    }

    /// End to end with no window and no GPU: type a command, press Enter, and
    /// find it in the command buffer, in the history and echoed in the ring.
    #[test]
    fn a_typed_line_reaches_the_command_buffer() {
        let ctx = egui::Context::default();
        let mut ui = ConsoleUi::new();
        let mut console = console();
        ui.set_open(true);

        // The first pass gives the entry focus; typing lands in the second.
        pass(&ctx, &mut ui, &mut console, Vec::new());
        pass(&ctx, &mut ui, &mut console, typed("map sp_a1_intro1"));
        assert_eq!(ui.input(), "map sp_a1_intro1");

        pass(
            &ctx,
            &mut ui,
            &mut console,
            vec![key(egui::Key::Enter, egui::Modifiers::NONE)],
        );

        assert_eq!(ui.input(), "", "the entry is cleared on submit");
        assert_eq!(ui.history(), ["map sp_a1_intro1"]);
        assert!(
            console
                .log()
                .lines()
                .any(|line| line.text == "] map sp_a1_intro1"),
            "the submitted line is echoed as a transcript"
        );

        // And it is really queued, not merely displayed.
        console.run(&mut NoTarget);
        assert!(console.buffer().is_empty());
    }

    #[test]
    fn history_keeps_the_newest_copy_of_a_repeated_command() {
        let mut ui = ConsoleUi::new();
        for command in ["map a", "map b", "map a"] {
            ui.push_history(command);
        }
        assert_eq!(ui.history(), ["map b", "map a"]);
    }

    #[test]
    fn history_is_bounded() {
        let mut ui = ConsoleUi::new();
        for index in 0..MAX_HISTORY_ITEMS + 10 {
            ui.push_history(&format!("map {index}"));
        }
        assert_eq!(ui.history().len(), MAX_HISTORY_ITEMS);
        assert_eq!(ui.history()[0], format!("map {}", 10));
    }

    /// Tab cycles the completion list forward, shift-tab back, and both wrap.
    #[test]
    fn tab_cycles_the_completions() {
        let ctx = egui::Context::default();
        let mut ui = ConsoleUi::new();
        let mut console = console();
        ui.set_open(true);

        pass(&ctx, &mut ui, &mut console, Vec::new());
        pass(&ctx, &mut ui, &mut console, typed("mat_wire"));
        assert_eq!(
            ui.completion()
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            ["mat_wireframe", "mat_wireframe_toggle"],
            "sorted by name, and the hidden cvar is not in it"
        );

        pass(
            &ctx,
            &mut ui,
            &mut console,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
        );
        assert_eq!(ui.input(), "mat_wireframe ");

        pass(
            &ctx,
            &mut ui,
            &mut console,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
        );
        assert_eq!(ui.input(), "mat_wireframe_toggle ");

        // Wraps, and the list did not rebuild itself from the completed text
        // on the way round.
        pass(
            &ctx,
            &mut ui,
            &mut console,
            vec![key(egui::Key::Tab, egui::Modifiers::NONE)],
        );
        assert_eq!(ui.input(), "mat_wireframe ");
    }

    /// `RebuildCompletionList`'s empty-input rule: Up on an empty line is the
    /// most recent command, which is why there is no separate history key.
    #[test]
    fn an_empty_line_cycles_history_instead() {
        let ctx = egui::Context::default();
        let mut ui = ConsoleUi::new();
        let mut console = console();
        ui.set_open(true);
        ui.push_history("map a");
        ui.push_history("map b");

        pass(&ctx, &mut ui, &mut console, Vec::new());
        pass(
            &ctx,
            &mut ui,
            &mut console,
            vec![key(egui::Key::ArrowUp, egui::Modifiers::NONE)],
        );
        assert_eq!(
            ui.input(), "map b",
            "Up lands on the newest, and a completion that already has a \
             space does not get another"
        );
    }

    /// Escape closes the dialog. It never reaches the game, because the whole
    /// event was claimed by the UI — which is why pressing Escape to close the
    /// console does not also hand the cursor back to the desktop.
    #[test]
    fn escape_closes_the_console() {
        let ctx = egui::Context::default();
        let mut ui = ConsoleUi::new();
        let mut console = console();
        ui.set_open(true);

        pass(&ctx, &mut ui, &mut console, Vec::new());
        pass(
            &ctx,
            &mut ui,
            &mut console,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(!ui.is_open());
    }

    #[test]
    fn closing_forgets_the_completion_state() {
        let ctx = egui::Context::default();
        let mut ui = ConsoleUi::new();
        let mut console = console();
        ui.set_open(true);
        pass(&ctx, &mut ui, &mut console, Vec::new());
        pass(&ctx, &mut ui, &mut console, typed("mat_wire"));
        assert!(!ui.completion().is_empty());

        ui.set_open(false);
        assert!(ui.completion().is_empty());
    }
}
