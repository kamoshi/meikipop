use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

use ashpd::desktop::Session;
use ashpd::desktop::global_shortcuts::GlobalShortcuts;
use futures_util::StreamExt;
use slint::Weak;

use crate::SettingsWindow;

struct ActiveSession {
    // The session is tied to this D-Bus connection, so both must stay alive.
    portal: GlobalShortcuts,
    session: Session<GlobalShortcuts>,
}

static ACTIVE_SESSION: Mutex<Option<ActiveSession>> = Mutex::new(None);
// Portal calls run on dedicated threads. Serialize session creation and chooser
// operations so a quick click during startup cannot create competing sessions.
static SESSION_OPERATION: Mutex<()> = Mutex::new(());
static CHOOSER_OPEN: AtomicBool = AtomicBool::new(false);
static SIGNAL_LISTENERS_STARTED: AtomicBool = AtomicBool::new(false);

/// Load the portal-approved binding without opening any UI.
pub fn initialize(settings_weak: Weak<SettingsWindow>) {
    std::thread::spawn(move || {
        let _operation = SESSION_OPERATION
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let mut active = take_active_session();
        let result = async_io::block_on(async {
            if active.is_none() {
                active = Some(new_session().await?);
            }
            let session = active.as_ref().expect("portal session was initialized");
            let response = session
                .portal
                .list_shortcuts(&session.session, Default::default())
                .await?
                .response()?;
            Ok::<_, ashpd::Error>(shortcut_description(response.shortcuts()))
        });
        restore_active_session(active);
        match result {
            Ok(description) => {
                start_signal_listeners(settings_weak.clone());
                update_settings(settings_weak, description);
            }
            Err(err) => tracing::warn!(%err, "Could not initialize GlobalShortcuts portal session"),
        }
    });
}

fn start_signal_listeners(settings_weak: Weak<SettingsWindow>) {
    if SIGNAL_LISTENERS_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let connection = match ACTIVE_SESSION
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_ref()
    {
        Some(active) => active.portal.connection().clone(),
        None => return,
    };
    for held in [true, false] {
        let connection = connection.clone();
        let settings_weak = settings_weak.clone();
        std::thread::spawn(move || {
            let result = async_io::block_on(async {
                let portal = GlobalShortcuts::with_connection(connection).await?;
                if held {
                    let mut signals = portal.receive_activated().await?;
                    while let Some(signal) = signals.next().await {
                        if signal.shortcut_id() == "scan_text" {
                            notify_hotkey_state(settings_weak.clone(), true);
                        }
                    }
                } else {
                    let mut signals = portal.receive_deactivated().await?;
                    while let Some(signal) = signals.next().await {
                        if signal.shortcut_id() == "scan_text" {
                            notify_hotkey_state(settings_weak.clone(), false);
                        }
                    }
                }
                Ok::<_, ashpd::Error>(())
            });
            if let Err(error) = result {
                tracing::warn!(%error, held, "Global shortcut signal listener stopped");
            }
        });
    }
}

/// Open the portal's first-time picker or its existing-binding editor.
pub fn choose(settings_weak: Weak<SettingsWindow>) {
    if CHOOSER_OPEN.swap(true, Ordering::AcqRel) {
        tracing::debug!("Global shortcut chooser is already open");
        return;
    }

    std::thread::spawn(move || {
        let _operation = SESSION_OPERATION
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        let mut active = take_active_session();
        let result = async_io::block_on(async {
            if active.is_none() {
                active = Some(new_session().await?);
            }
            let session = active.as_ref().expect("portal session was initialized");
            let existing = session
                .portal
                .list_shortcuts(&session.session, Default::default())
                .await?
                .response()?;

            if existing
                .shortcuts()
                .iter()
                .any(|shortcut| shortcut.id() == "scan_text")
            {
                // BindShortcuts is allowed once per session. Version 2 uses
                // ConfigureShortcuts to edit or clear an existing binding.
                if session.portal.version() >= 2 {
                    session
                        .portal
                        .configure_shortcuts(&session.session, None, Default::default())
                        .await?;
                    let response = session
                        .portal
                        .list_shortcuts(&session.session, Default::default())
                        .await?
                        .response()?;
                    Ok::<_, ashpd::Error>(Some(shortcut_description(response.shortcuts())))
                } else {
                    // v1 has no ConfigureShortcuts or unbind operation.
                    // Recreating the session merely restores the saved binding
                    // without opening a picker, so do not pretend it worked.
                    tracing::warn!(
                        version = session.portal.version(),
                        "This GlobalShortcuts portal cannot edit an existing binding; use the desktop's shortcut settings or upgrade to portal version 2"
                    );
                    show_legacy_portal_notice(settings_weak.clone());
                    Ok::<_, ashpd::Error>(None)
                }
            } else {
                bind_shortcut(session).await.map(Some)
            }
        });
        restore_active_session(active);
        CHOOSER_OPEN.store(false, Ordering::Release);
        match result {
            Ok(Some(description)) => {
                start_signal_listeners(settings_weak.clone());
                tracing::info!(shortcut = %description, "Global shortcut chooser closed");
                update_settings(settings_weak, description);
            }
            Ok(None) => {}
            Err(err) => tracing::warn!(%err, "Could not open global shortcut chooser"),
        }
    });
}

async fn new_session() -> Result<ActiveSession, ashpd::Error> {
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session(Default::default()).await?;
    Ok(ActiveSession { portal, session })
}

async fn bind_shortcut(active: &ActiveSession) -> Result<String, ashpd::Error> {
    use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, NewShortcut};

    let shortcut =
        NewShortcut::new("scan_text", "Scan Text Under Cursor").preferred_trigger("Alt+S");
    let response = active
        .portal
        .bind_shortcuts(
            &active.session,
            &[shortcut],
            None,
            BindShortcutsOptions::default(),
        )
        .await?
        .response()?;
    Ok(shortcut_description(response.shortcuts()))
}

fn take_active_session() -> Option<ActiveSession> {
    ACTIVE_SESSION
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take()
}

fn restore_active_session(active: Option<ActiveSession>) {
    *ACTIVE_SESSION.lock().unwrap_or_else(|err| err.into_inner()) = active;
}

fn shortcut_description(shortcuts: &[ashpd::desktop::global_shortcuts::Shortcut]) -> String {
    shortcuts
        .iter()
        .find(|shortcut| shortcut.id() == "scan_text")
        .map(|shortcut| shortcut.trigger_description().to_owned())
        .unwrap_or_default()
}

fn update_settings(settings_weak: Weak<SettingsWindow>, description: String) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(settings) = settings_weak.upgrade() {
            settings.set_hotkey(description.into());
        }
    });
}

fn notify_hotkey_state(settings_weak: Weak<SettingsWindow>, held: bool) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(settings) = settings_weak.upgrade() {
            settings.invoke_hotkey_held(held);
        }
    });
}

fn show_legacy_portal_notice(settings_weak: Weak<SettingsWindow>) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(settings) = settings_weak.upgrade() {
            settings.set_show_hotkey_portal_notice(true);
        }
    });
}
