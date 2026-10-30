// SPDX-License-Identifier: Apache-2.0
pub mod alerts;
pub mod backoff;
mod clip;
mod delivery;
pub mod device;
pub mod error;
mod ipc;
pub mod keys;
pub mod links;
pub mod network;
pub mod outbox;
pub mod preview;
pub mod profiles;
mod reports;
use std::sync::Arc;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .channel_interceptor(delivery::deliver)
        .plugin(tauri_plugin_device::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let device = device::AndroidDevice(app.handle().clone());
            let data = app.path().app_data_dir()?;
            let pending = reports::PendingCrashes::new(data.clone());
            if reports::mobile_reports() {
                pending.install(app.package_info().version.to_string());
            }
            app.manage(pending);
            let profiles = profiles::Profiles::open(data.clone());
            let native = Arc::new(device.clone());
            let links = Arc::new(links::Links::new(
                native.clone(),
                native,
                Arc::new(ports::SystemClock),
            ));
            alerts::init(&data);
            alerts::attach(&links);
            app.manage(ipc::AppCore {
                profiles: tokio::sync::Mutex::new(profiles),
                operations: tokio::sync::Mutex::new(()),
                pairing: std::sync::Mutex::new(None),
                outbox: outbox::Outbox::new(data),
                links,
                device,
                previews: Arc::new(preview::Previews::default()),
            });
            Ok(())
        })
        .on_page_load(|webview, load| {
            if load.event() == tauri::webview::PageLoadEvent::Started {
                webview.state::<ipc::AppCore>().page_started();
            }
        })
        .invoke_handler(tauri::generate_handler![
            ipc::profiles,
            ipc::pair,
            ipc::pair_cancel,
            ipc::rename_profile,
            ipc::forget,
            ipc::outbox_put,
            ipc::outbox_list,
            ipc::outbox_remove,
            ipc::link_page,
            ipc::link_open,
            ipc::link_connect,
            ipc::link_send,
            ipc::link_send_binary,
            ipc::link_sent,
            ipc::link_close,
            ipc::device_events,
            ipc::share_text,
            ipc::shared_read,
            ipc::shared_ack,
            ipc::app_update_check,
            ipc::app_update_begin,
            ipc::app_update_chunk,
            ipc::app_update_commit,
            ipc::app_update_cancel,
            ipc::save_begin,
            ipc::save_chunk,
            ipc::save_finish,
            ipc::save_cancel,
            ipc::notify,
            ipc::cancel_notifications,
            ipc::request_notifications,
            ipc::app_info,
            ipc::keep_awake,
            ipc::alerts_state,
            ipc::alerts_configure,
            ipc::alerts_seen,
            ipc::alerts_viewing,
            ipc::alerts_battery,
            ipc::alerts_channel,
            ipc::scan_pairing_code,
            ipc::haptic,
            ipc::shell_theme,
            ipc::shell_immersive,
            ipc::app_lock_state,
            ipc::app_lock_configure,
            ipc::app_unlock,
            ipc::app_confirm,
            ipc::app_lock,
            ipc::app_lock_screen_ready,
            preview::preview_open,
            preview::preview_close,
            preview::preview_show,
            preview::preview_picture_read,
            preview::preview_picture_ack,
            preview::preview_pending,
            ipc::notification_permission,
            clip::clip_begin,
            clip::clip_write,
            clip::clip_trim,
            clip::clip_read,
            clip::clip_discard,
            reports::take_native_crashes,
            reports::screenshot_supported,
            reports::start_screenshot_watch,
            reports::stop_screenshot_watch,
            reports::capture_own_view,
        ])
        .build(tauri::generate_context!())
        .expect("start app")
        .run(|_, event| {
            #[cfg(target_os = "android")]
            if let tauri::RunEvent::Exit = event {
                exit_now();
            }
            #[cfg(not(target_os = "android"))]
            let _ = event;
        });
}

/// Ends the process once the app's window has closed, as when it is swiped
/// away.
///
/// Tauri would call `exit`, whose C++ cleanup can tear down Android's drawing
/// threads while they still run. The app then crashes as it closes: Android
/// says it keeps stopping, and waits up to half an hour before restarting the
/// alert service. `_exit` ends the process without that cleanup.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
fn exit_now() -> ! {
    // SAFETY: `_exit` takes no pointers and does not return; nothing of ours
    // runs at exit, and other threads stop as they would under `exit`.
    unsafe { libc::_exit(0) }
}
