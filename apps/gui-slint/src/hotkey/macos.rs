//! macOS global hold-hotkey support.
//!
//! A listen-only `CGEventTap` observes key transitions without modifying or
//! suppressing them. macOS protects this with Input Monitoring permission.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
    CGEventTapPlacement, CGEventTapProxy, CGEventType, CGPreflightListenEventAccess,
    CGRequestListenEventAccess,
};

use super::HotkeyEvents;

const SHORTCUT_DESCRIPTION: &str = "⇧⌘C";
const KEY_CODE_C: i64 = 8;
const REQUIRED_FLAGS: CGEventFlags = CGEventFlags::MaskShift.union(CGEventFlags::MaskCommand);

static LISTENER_STARTED: AtomicBool = AtomicBool::new(false);

struct TapState {
    events: HotkeyEvents,
    tap: Option<CFRetained<CFMachPort>>,
    held: bool,
}

pub fn initialize(events: HotkeyEvents) {
    events.binding_changed(SHORTCUT_DESCRIPTION.to_owned());

    // Do not surprise the user with a permission prompt at startup. If access
    // was granted previously, resume listening immediately.
    if CGPreflightListenEventAccess() {
        start_listener(events);
    }
}

pub fn choose(events: HotkeyEvents) {
    events.binding_changed(SHORTCUT_DESCRIPTION.to_owned());

    // This call is made from an explicit click in Settings, which is the right
    // time to let macOS present its Input Monitoring consent UI.
    if CGPreflightListenEventAccess() || CGRequestListenEventAccess() {
        start_listener(events.clone());
        events.show_info(
            "Global shortcut enabled",
            "MeikiPop can now scan while you hold ⇧⌘C. You can review this permission in System Settings → Privacy & Security → Input Monitoring.",
        );
    } else {
        events.show_info(
            "Input Monitoring is required",
            "Allow MeikiPop in System Settings → Privacy & Security → Input Monitoring, then return here and click the shortcut again.",
        );
    }
}

fn start_listener(events: HotkeyEvents) {
    if LISTENER_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }

    if let Err(error) = std::thread::Builder::new()
        .name("GlobalHotkey".to_owned())
        .spawn(move || run_event_tap(events))
    {
        LISTENER_STARTED.store(false, Ordering::Release);
        tracing::warn!(%error, "Could not start macOS global-hotkey thread");
    }
}

fn run_event_tap(events: HotkeyEvents) {
    let state = Box::new(TapState {
        events,
        tap: None,
        held: false,
    });
    let state = Box::into_raw(state);
    const EVENT_MASK: CGEventMask = (1 << CGEventType::KeyDown.0)
        | (1 << CGEventType::KeyUp.0)
        | (1 << CGEventType::FlagsChanged.0);

    // SAFETY: `state` remains allocated for the lifetime of the run loop and
    // the callback uses it only on this thread. The callback returns each
    // borrowed event unchanged because this is a listen-only tap.
    let tap = unsafe {
        CGEvent::tap_create(
            CGEventTapLocation::SessionEventTap,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::ListenOnly,
            EVENT_MASK,
            Some(event_tap_callback),
            state.cast(),
        )
    };
    let Some(tap) = tap else {
        // SAFETY: ownership was transferred by `Box::into_raw` above and the
        // callback cannot run when tap creation failed.
        unsafe { drop(Box::from_raw(state)) };
        LISTENER_STARTED.store(false, Ordering::Release);
        tracing::warn!("Could not create macOS event tap; check Input Monitoring permission");
        return;
    };
    unsafe { (*state).tap = Some(tap.clone()) };

    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0) else {
        unsafe { drop(Box::from_raw(state)) };
        LISTENER_STARTED.store(false, Ordering::Release);
        tracing::warn!("Could not create run-loop source for macOS event tap");
        return;
    };

    let Some(run_loop) = CFRunLoop::current() else {
        unsafe { drop(Box::from_raw(state)) };
        LISTENER_STARTED.store(false, Ordering::Release);
        tracing::warn!("Could not get current run loop for macOS event tap");
        return;
    };

    run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
    CGEvent::tap_enable(&tap, true);
    CFRunLoop::run();

    unsafe { drop(Box::from_raw(state)) };
    LISTENER_STARTED.store(false, Ordering::Release);
}

unsafe extern "C-unwind" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: CGEventType,
    event: NonNull<CGEvent>,
    user_info: *mut c_void,
) -> *mut CGEvent {
    // SAFETY: `user_info` points to the `TapState` owned by `run_event_tap`.
    let state = unsafe { &mut *user_info.cast::<TapState>() };

    if matches!(
        event_type,
        CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
    ) {
        set_held(state, false);
        if let Some(tap) = &state.tap {
            CGEvent::tap_enable(tap, true);
        }
        return event.as_ptr();
    }

    let event_ref = unsafe { event.as_ref() };
    let flags = CGEvent::flags(Some(event_ref));
    let modifiers_held = flags.contains(REQUIRED_FLAGS);
    match event_type {
        CGEventType::KeyDown
            if modifiers_held
                && CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode)
                    == KEY_CODE_C =>
        {
            set_held(state, true);
        }
        CGEventType::KeyUp
            if CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode)
                == KEY_CODE_C =>
        {
            set_held(state, false);
        }
        CGEventType::FlagsChanged if !modifiers_held => set_held(state, false),
        _ => {}
    }
    event.as_ptr()
}

fn set_held(state: &mut TapState, held: bool) {
    if state.held != held {
        state.held = held;
        state.events.held_changed(held);
    }
}
