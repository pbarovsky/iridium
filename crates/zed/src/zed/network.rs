//! The user-facing side of Zed's network policy (see
//! [`http_client::network_guard`]): notifications asking whether Zed may
//! download something for the first time, and a status bar indicator while
//! Zed is offline.

use std::sync::Arc;

use collections::HashMap;
use fs::Fs;
use futures::{
    FutureExt as _, StreamExt as _,
    channel::{mpsc, oneshot},
};
use gpui::{App, AppContext as _, Context, ElementId, IntoElement, Render, Subscription, Window};
use http_client::network_guard;
use parking_lot::Mutex;
use settings::{SettingsStore, update_settings_file};
use ui::{Tooltip, prelude::*};
use workspace::{
    HideStatusItem, ItemHandle, StatusItemView,
    notifications::{
        NotificationId, show_app_notification, simple_message_notification::MessageNotification,
    },
};

struct DownloadPrompt;

/// Answers to download questions, kept for the session so the same question
/// isn't asked again, and the callers still waiting on unanswered ones.
#[derive(Default)]
struct DownloadAnswers {
    answered: HashMap<String, bool>,
    waiting: HashMap<String, Vec<oneshot::Sender<bool>>>,
}

/// One question on screen. If its notification is closed without an answer,
/// dropping it tells whoever is waiting that the download wasn't allowed.
struct OpenPrompt {
    subject: String,
    answers: Arc<Mutex<DownloadAnswers>>,
}

impl OpenPrompt {
    fn answer(&self, allowed: bool) {
        let waiting = {
            let mut answers = self.answers.lock();
            answers.answered.insert(self.subject.clone(), allowed);
            answers.waiting.remove(&self.subject).unwrap_or_default()
        };
        for sender in waiting {
            sender.send(allowed).ok();
        }
    }
}

impl Drop for OpenPrompt {
    fn drop(&mut self) {
        let waiting = self.answers.lock().waiting.remove(&self.subject);
        for sender in waiting.into_iter().flatten() {
            sender.send(false).ok();
        }
    }
}

pub fn init(cx: &mut App) {
    let (request_tx, mut request_rx) = mpsc::unbounded::<(String, oneshot::Sender<bool>)>();
    network_guard::set_download_consent_handler(Arc::new(move |subject| {
        let (answer_tx, answer_rx) = oneshot::channel();
        let sent = request_tx.unbounded_send((subject, answer_tx)).is_ok();
        async move { sent && answer_rx.await.unwrap_or(false) }.boxed()
    }));

    let answers = Arc::new(Mutex::new(DownloadAnswers::default()));
    cx.spawn(async move |cx| {
        while let Some((subject, answer_tx)) = request_rx.next().await {
            {
                let mut state = answers.lock();
                if let Some(allowed) = state.answered.get(&subject).copied() {
                    answer_tx.send(allowed).ok();
                    continue;
                }
                // The same question is already on screen; wait for its answer.
                if let Some(waiting) = state.waiting.get_mut(&subject) {
                    waiting.push(answer_tx);
                    continue;
                }
                state.waiting.insert(subject.clone(), vec![answer_tx]);
            }
            let prompt = Arc::new(OpenPrompt {
                subject,
                answers: answers.clone(),
            });
            cx.update(|cx| show_download_prompt(prompt, cx));
        }
    })
    .detach();
}

fn show_download_prompt(prompt: Arc<OpenPrompt>, cx: &mut App) {
    let id =
        NotificationId::composite::<DownloadPrompt>(ElementId::Name(prompt.subject.clone().into()));
    show_app_notification(id, cx, move |cx| {
        let prompt = prompt.clone();
        cx.new(|cx| {
            MessageNotification::new(format!("Zed wants to download {}.", prompt.subject), cx)
                .primary_message("Download")
                .primary_icon(IconName::Download)
                .primary_on_click({
                    let prompt = prompt.clone();
                    move |_, _| prompt.answer(true)
                })
                .secondary_message("Not Now")
                .secondary_on_click(move |_, _| prompt.answer(false))
        })
    });
}

/// Shows that Zed is offline, in the status bar. Clicking it goes online,
/// unless Zed was started offline.
pub struct OfflineIndicator {
    fs: Arc<dyn Fs>,
    _settings_subscription: Subscription,
}

impl OfflineIndicator {
    pub fn new(fs: Arc<dyn Fs>, cx: &mut Context<Self>) -> Self {
        Self {
            fs,
            _settings_subscription: cx.observe_global::<SettingsStore>(|_, cx| cx.notify()),
        }
    }
}

impl Render for OfflineIndicator {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        if !network_guard::is_offline() {
            return div().into_any_element();
        }
        let forced = network_guard::is_forced_offline();
        let fs = self.fs.clone();
        Button::new("offline-indicator", "Offline")
            .label_size(LabelSize::Small)
            .color(Color::Warning)
            .start_icon(
                Icon::new(IconName::Disconnected)
                    .size(IconSize::Small)
                    .color(Color::Warning),
            )
            .tooltip(Tooltip::text(if forced {
                "Zed was started offline, with --offline or ZED_OFFLINE, until it restarts."
            } else {
                "Zed is offline: nothing is downloaded or checked. Click to go online."
            }))
            .when(!forced, |button| {
                button.on_click(move |_, _, cx| {
                    update_settings_file(fs.clone(), cx, |settings, _| {
                        settings.network.get_or_insert_default().offline = Some(false);
                    });
                })
            })
            .into_any_element()
    }
}

impl StatusItemView for OfflineIndicator {
    fn set_active_pane_item(
        &mut self,
        _active_pane_item: Option<&dyn ItemHandle>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }

    fn hide_setting(&self, _: &App) -> Option<HideStatusItem> {
        None
    }
}
