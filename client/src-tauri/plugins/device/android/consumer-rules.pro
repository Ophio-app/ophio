# SPDX-License-Identifier: Apache-2.0
# Tauri loads this class by name and calls annotated commands through reflection.
-keep @app.tauri.annotation.TauriPlugin class app.ophio.device.DevicePlugin { *; }
-keep @app.tauri.annotation.InvokeArg class app.ophio.device.** { *; }
-keepattributes RuntimeVisibleAnnotations,AnnotationDefault
# Rust calls the alert watcher's callbacks and native methods by name.
-keep class app.ophio.device.AlertWatcher { *; }
