use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, Wry,
};

use crate::status::{StatusKind, StatusSnapshot};

const TRAY_ID: &str = "main";

pub(crate) fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn status_color(kind: StatusKind) -> (u8, u8, u8) {
    match kind {
        StatusKind::NoProject | StatusKind::NotWatching => (138, 151, 176), // gray
        StatusKind::Initializing | StatusKind::Checking => (79, 140, 255),  // blue
        StatusKind::Healthy => (52, 211, 153),                              // green
        StatusKind::Warnings | StatusKind::Unavailable => (245, 165, 36),   // amber
        StatusKind::Errors => (255, 107, 107),                              // red
    }
}

/// A filled colour dot with a soft edge. Colour is never the only signal:
/// the tooltip and the first menu line always carry the status as text.
fn make_icon(kind: StatusKind) -> Image<'static> {
    const SIZE: u32 = 32;
    let (r, g, b) = status_color(kind);
    let center = (SIZE as f32 - 1.0) / 2.0;
    let radius = center - 1.0;

    let mut rgba: Vec<u8> = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let coverage = (radius - distance + 0.5).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[r, g, b, (coverage * 255.0) as u8]);
        }
    }

    Image::new_owned(rgba, SIZE, SIZE)
}

fn build_menu(app: &AppHandle, status_label: &str) -> tauri::Result<Menu<Wry>> {
    let status_item = MenuItem::with_id(
        app,
        "status",
        format!("Status: {status_label}"),
        false,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let open_item = MenuItem::with_id(app, "open", "Open dashboard", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    Menu::with_items(app, &[&status_item, &separator, &open_item, &quit_item])
}

pub(crate) fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let menu = build_menu(app.handle(), "No project")?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(make_icon(StatusKind::NoProject))
        .tooltip("AI BuildGuard — No project")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

/// Called by the status manager whenever the status changes.
pub(crate) fn update_tray(app: &AppHandle, snapshot: &StatusSnapshot) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    let _ = tray.set_icon(Some(make_icon(snapshot.kind)));
    let _ = tray.set_tooltip(Some(format!("AI BuildGuard — {}", snapshot.label)));

    if let Ok(menu) = build_menu(app, &snapshot.label) {
        let _ = tray.set_menu(Some(menu));
    }
}