// SPDX-License-Identifier: Apache-2.0
//! Android operations are reachable only through the shell's narrow commands.
use serde::{Serialize, de::DeserializeOwned};
use tauri::{
    Manager, Runtime,
    plugin::{Builder, TauriPlugin},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("This operation requires Android.")]
    Unsupported,
    #[error("{0}")]
    Native(String),
}

pub struct Device<R: Runtime> {
    #[cfg(target_os = "android")]
    handle: tauri::plugin::PluginHandle<R>,
    #[cfg(not(target_os = "android"))]
    marker: std::marker::PhantomData<fn() -> R>,
}

impl<R: Runtime> Device<R> {
    pub async fn call<T: DeserializeOwned + Send + 'static>(
        &self,
        command: &str,
        payload: impl Serialize,
    ) -> Result<T, Error> {
        #[cfg(target_os = "android")]
        {
            let handle = self.handle.clone();
            let command = command.to_owned();
            let payload = serde_json::to_value(payload)
                .map_err(|_| Error::Native("Invalid device request.".into()))?;
            // Keep the native response receiver alive if a link is cancelled.
            // Tauri's async mobile callback assumes its receiver still exists.
            tauri::async_runtime::spawn_blocking(move || handle.run_mobile_plugin(command, payload))
                .await
                .map_err(|_| Error::Native("The device operation stopped.".into()))?
                .map_err(|error| match error {
                    tauri::plugin::mobile::PluginInvokeError::InvokeRejected(response) => {
                        Error::Native(response.message.unwrap_or_else(|| {
                            "The phone could not complete this operation.".into()
                        }))
                    }
                    _ => Error::Native("The phone could not complete this operation.".into()),
                })
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (command, payload);
            Err(Error::Unsupported)
        }
    }
}

pub trait DeviceExt<R: Runtime> {
    fn device(&self) -> &Device<R>;
}
impl<R: Runtime, T: Manager<R>> DeviceExt<R> for T {
    fn device(&self) -> &Device<R> {
        self.state::<Device<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("device")
        .on_navigation(|_, url| {
            matches!(
                (url.scheme(), url.host_str()),
                ("tauri", Some("localhost")) | ("http" | "https", Some("tauri.localhost"))
            )
        })
        .setup(|app, _api| {
            #[cfg(target_os = "android")]
            let device = Device {
                handle: _api.register_android_plugin("app.ophio.device", "DevicePlugin")?,
            };
            #[cfg(not(target_os = "android"))]
            let device = Device::<R> {
                marker: std::marker::PhantomData,
            };
            app.manage(device);
            Ok(())
        })
        .build()
}
