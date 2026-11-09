use super::types::{ComposerPhase, MessageAttempt, MessageComposer};
use super::{App, AppMode, DialogMode};
use crate::live_bridge::{self, Delivery, LiveTarget, MAX_MESSAGE_BYTES, SendFailure};
use crossterm::event::{KeyCode, KeyModifiers};
use std::sync::mpsc;

pub(super) enum MessageJob {
    Connected(Result<LiveTarget, String>),
    Sent(Result<String, SendFailure>),
}

impl App {
    pub(super) fn start_message_composer(&mut self) {
        let AppMode::View(state) = &self.app_mode else {
            return;
        };
        self.dialog_mode = DialogMode::MessageComposer(Box::new(MessageComposer {
            session_file: state.conversation_path.clone(),
            input: String::new(),
            cursor: 0,
            target: None,
            error: None,
            phase: ComposerPhase::Ready,
            last_attempt: None,
        }));
        self.connect_message_target();
    }

    fn connect_message_target(&mut self) {
        let DialogMode::MessageComposer(composer) = &mut self.dialog_mode else {
            return;
        };
        composer.phase = ComposerPhase::Connecting;
        composer.error = None;
        let path = composer.session_file.clone();
        let (tx, rx) = mpsc::channel();
        self.message_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(MessageJob::Connected(live_bridge::discover(&path)));
        });
    }

    pub fn has_message_work(&self) -> bool {
        self.message_rx.is_some()
    }

    pub fn receive_message_results(&mut self) {
        let Some(rx) = &self.message_rx else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                if matches!(&self.dialog_mode, DialogMode::MessageComposer(composer) if composer.phase == ComposerPhase::Sending)
                {
                    MessageJob::Sent(Err(SendFailure {
                        message: "Message bridge worker stopped".to_string(),
                        uncertain: true,
                    }))
                } else {
                    MessageJob::Connected(Err("Message bridge worker stopped".to_string()))
                }
            }
        };
        self.message_rx = None;
        let DialogMode::MessageComposer(composer) = &mut self.dialog_mode else {
            return;
        };
        composer.phase = ComposerPhase::Ready;
        match result {
            MessageJob::Connected(Ok(target)) => {
                composer.target = Some(target);
                composer.error = None;
            }
            MessageJob::Connected(Err(error)) => {
                composer.target = None;
                composer.error = Some(error);
            }
            MessageJob::Sent(Ok(message)) => {
                self.dialog_mode = DialogMode::None;
                self.status_message = Some((message, std::time::Instant::now()));
            }
            MessageJob::Sent(Err(error)) => {
                composer.error = Some(if error.uncertain {
                    format!(
                        "{}. Delivery may already have succeeded. Check Pi before retrying; unchanged retries use the same ID.",
                        error.message
                    )
                } else {
                    error.message
                });
            }
        }
    }

    pub fn handle_message_paste(&mut self, text: &str) {
        let DialogMode::MessageComposer(composer) = &mut self.dialog_mode else {
            return;
        };
        if composer.phase == ComposerPhase::Sending {
            return;
        }
        let clean: String = text
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect();
        if composer.input.len() + clean.len() > MAX_MESSAGE_BYTES {
            composer.error = Some("Message limit is 32 KiB; paste was not inserted".to_string());
            return;
        }
        let byte = composer
            .input
            .char_indices()
            .nth(composer.cursor)
            .map_or(composer.input.len(), |(i, _)| i);
        composer.input.insert_str(byte, &clean);
        composer.cursor += clean.chars().count();
    }

    fn submit_message(&mut self, delivery: Delivery) {
        let DialogMode::MessageComposer(composer) = &mut self.dialog_mode else {
            return;
        };
        if composer.phase != ComposerPhase::Ready {
            return;
        }
        if composer.target.is_none() {
            self.connect_message_target();
            return;
        }
        if composer.input.trim().is_empty() {
            composer.error = Some("Write a message first".to_string());
            return;
        }
        let target = composer.target.clone().unwrap();
        let message = composer.input.clone();
        let request_id = composer
            .last_attempt
            .as_ref()
            .filter(|attempt| attempt.message == message && attempt.delivery == delivery)
            .map(|attempt| attempt.request_id.clone())
            .unwrap_or_else(live_bridge::new_request_id);
        composer.last_attempt = Some(MessageAttempt {
            message: message.clone(),
            delivery,
            request_id: request_id.clone(),
        });
        composer.phase = ComposerPhase::Sending;
        composer.error = None;
        let (tx, rx) = mpsc::channel();
        self.message_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(MessageJob::Sent(live_bridge::send(
                &target,
                &request_id,
                &message,
                delivery,
            )));
        });
    }

    pub(super) fn handle_message_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<super::Action> {
        let DialogMode::MessageComposer(composer) = &mut self.dialog_mode else {
            return None;
        };
        // Once a request is in flight, Escape cannot cancel delivery already accepted by Pi.
        if composer.phase == ComposerPhase::Sending {
            return None;
        }
        match code {
            KeyCode::Esc => {
                self.dialog_mode = DialogMode::None;
                self.message_rx = None;
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.dialog_mode = DialogMode::None;
                self.message_rx = None;
            }
            KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => {
                self.handle_message_paste("\n")
            }
            KeyCode::Char('j') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.handle_message_paste("\n")
            }
            KeyCode::Enter if modifiers.contains(KeyModifiers::ALT) => {
                self.submit_message(Delivery::FollowUp)
            }
            KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.submit_message(Delivery::FollowUp)
            }
            KeyCode::Enter => self.submit_message(Delivery::Steer),
            KeyCode::Left => composer.cursor = composer.cursor.saturating_sub(1),
            KeyCode::Right => {
                composer.cursor = (composer.cursor + 1).min(composer.input.chars().count())
            }
            KeyCode::Home => composer.cursor = 0,
            KeyCode::End => composer.cursor = composer.input.chars().count(),
            KeyCode::Up | KeyCode::Down => {
                let chars: Vec<_> = composer.input.chars().collect();
                let start = chars[..composer.cursor]
                    .iter()
                    .rposition(|c| *c == '\n')
                    .map_or(0, |i| i + 1);
                let column = composer.cursor - start;
                if code == KeyCode::Up && start > 0 {
                    let previous = chars[..start - 1]
                        .iter()
                        .rposition(|c| *c == '\n')
                        .map_or(0, |i| i + 1);
                    composer.cursor = previous + column.min(start - 1 - previous);
                } else if code == KeyCode::Down
                    && let Some(end) = chars[composer.cursor..].iter().position(|c| *c == '\n')
                {
                    let next = composer.cursor + end + 1;
                    let length = chars[next..]
                        .iter()
                        .position(|c| *c == '\n')
                        .unwrap_or(chars.len() - next);
                    composer.cursor = next + column.min(length);
                }
            }
            KeyCode::Backspace if composer.cursor > 0 => {
                let byte = composer
                    .input
                    .char_indices()
                    .nth(composer.cursor - 1)
                    .unwrap()
                    .0;
                composer.input.remove(byte);
                composer.cursor -= 1;
            }
            KeyCode::Delete if composer.cursor < composer.input.chars().count() => {
                let byte = composer
                    .input
                    .char_indices()
                    .nth(composer.cursor)
                    .unwrap()
                    .0;
                composer.input.remove(byte);
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                composer.input.clear();
                composer.cursor = 0;
            }
            KeyCode::Char('k') if modifiers.contains(KeyModifiers::CONTROL) => {
                let byte = composer
                    .input
                    .char_indices()
                    .nth(composer.cursor)
                    .map_or(composer.input.len(), |(i, _)| i);
                composer.input.truncate(byte);
            }
            KeyCode::Char(c)
                if !modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.handle_message_paste(&c.to_string())
            }
            _ => {}
        }
        None
    }
}
