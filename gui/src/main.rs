use std::error::Error;

use slint::ComponentHandle;

slint::include_modules!();

fn main() -> Result<(), Box<dyn Error>> {
    let popup = OcrPopup::new()?;
    let tray = MeikiPopTray::new()?;

    let popup_weak = popup.as_weak();
    tray.on_show_popup(move || {
        if let Some(popup) = popup_weak.upgrade() {
            let _ = popup.show();
            popup
                .window()
                .set_position(slint::PhysicalPosition::new(80, 80));
        }
    });
    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    tray.show()?;
    slint::run_event_loop()?;

    Ok(())
}
