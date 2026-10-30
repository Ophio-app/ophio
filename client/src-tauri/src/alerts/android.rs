// SPDX-License-Identifier: Apache-2.0
//! Android's side of the watcher. The background service starts it without
//! the app's screen, so there is no Tauri plugin to call: it reaches Kotlin
//! through `app.ophio.device.AlertWatcher` directly. An answer from a
//! question's notification comes back the same way.
// JNI entry points must be exported under the names Java looks for.
#![allow(unsafe_code)]
use super::{Note, Posted, Shade};
use crate::{
    error::{Error, Result},
    keys::KeyVault,
};
use async_trait::async_trait;
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JByteArray, JClass, JObjectArray, JString, JValue},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jstring},
};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
};
use zeroize::Zeroizing;

struct Java {
    vm: JavaVM,
    /// `AlertWatcher`, which a thread Rust started cannot look up by name.
    class: GlobalRef,
}
static JAVA: OnceLock<Java> = OnceLock::new();

/// Keeps `AlertWatcher` for the threads Rust starts, the first time Kotlin
/// calls in.
fn attach(env: &mut JNIEnv, class: &JClass) -> Result<()> {
    if JAVA.get().is_none() {
        let native = |e: jni::errors::Error| Error::Native(e.to_string());
        let java = Java {
            vm: env.get_java_vm().map_err(native)?,
            class: env.new_global_ref(class).map_err(native)?,
        };
        let _ = JAVA.set(java);
    }
    Ok(())
}

fn call<T>(f: impl FnOnce(&mut JNIEnv, &GlobalRef) -> jni::errors::Result<T>) -> Result<T> {
    let java = JAVA
        .get()
        .ok_or_else(|| Error::Native("Android has not started the alert watcher.".into()))?;
    let mut env = java
        .vm
        .attach_current_thread_permanently()
        .map_err(|e| Error::Native(e.to_string()))?;
    env.with_local_frame(16, |env| {
        let result = f(env, &java.class);
        // The Kotlin side catches its own, so this is a JNI failure.
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        result
    })
    .map_err(|e| Error::Native(e.to_string()))
}

struct Android;
impl Shade for Android {
    fn post(&self, note: &Note, since: Option<i64>) -> Result<Posted> {
        let json = serde_json::to_string(note).map_err(|_| Error::Invalid("Invalid alert."))?;
        let posted = call(|env, class| {
            let json = env.new_string(json)?;
            env.call_static_method(
                class,
                "post",
                "(Ljava/lang/String;J)I",
                &[JValue::Object(&json), JValue::Long(since.unwrap_or(-1))],
            )?
            .i()
        })?;
        // As AlertWatcher.post answers.
        match posted {
            0 => Ok(Posted::Shown),
            1 => Ok(Posted::NoRoom),
            3 => Ok(Posted::Cleared),
            _ => Err(Error::Native("Android did not show the alert.".into())),
        }
    }
    fn cancel(&self, tag: &str) {
        let _ = call(|env, class| {
            let tag = env.new_string(tag)?;
            env.call_static_method(
                class,
                "cancel",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&tag)],
            )?
            .v()
        });
    }
    fn clears(&self) -> i64 {
        call(|env, class| env.call_static_method(class, "clears", "()J", &[])?.j()).unwrap_or(-1)
    }
    fn shown(&self) -> Vec<String> {
        call(|env, class| {
            let tags = JObjectArray::from(
                env.call_static_method(class, "shown", "()[Ljava/lang/String;", &[])?
                    .l()?,
            );
            let mut out = Vec::new();
            if tags.is_null() {
                return Ok(out);
            }
            for i in 0..env.get_array_length(&tags)? {
                let tag = JString::from(env.get_object_array_element(&tags, i)?);
                out.push(env.get_string(&tag)?.into());
                env.delete_local_ref(tag)?;
            }
            Ok(out)
        })
        .unwrap_or_default()
    }
    fn on_screen(&self) -> bool {
        call(|env, class| env.call_static_method(class, "onScreen", "()Z", &[])?.z())
            .unwrap_or(false)
    }
    fn app_updates(&self, profile_id: &str, name: &str, offers: &[protocol::capability::AppOffer]) {
        let Ok(offers) = serde_json::to_string(offers) else {
            return;
        };
        let _ = call(|env, class| {
            let profile_id = env.new_string(profile_id)?;
            let name = env.new_string(name)?;
            let offers = env.new_string(offers)?;
            env.call_static_method(
                class,
                "appUpdates",
                "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V",
                &[
                    JValue::Object(&profile_id),
                    JValue::Object(&name),
                    JValue::Object(&offers),
                ],
            )?
            .v()
        });
    }
}

/// The device keys in Android Keystore. The watcher only signs in with them.
struct Keys;
#[async_trait]
impl KeyVault for Keys {
    async fn generate(&self, _: &str) -> Result<Option<[u8; 32]>> {
        Err(Error::Key)
    }
    async fn sign(&self, alias: &str, transcript: &[u8]) -> Result<[u8; 64]> {
        let (alias, transcript) = (alias.to_owned(), transcript.to_owned());
        let signature = tokio::task::spawn_blocking(move || {
            call(|env, class| {
                let alias = env.new_string(alias)?;
                let transcript = env.byte_array_from_slice(&transcript)?;
                let signature = JByteArray::from(
                    env.call_static_method(
                        class,
                        "sign",
                        "(Ljava/lang/String;[B)[B",
                        &[JValue::Object(&alias), JValue::Object(&transcript)],
                    )?
                    .l()?,
                );
                if signature.is_null() {
                    return Ok(None);
                }
                env.convert_byte_array(&signature).map(Some)
            })
        })
        .await
        .map_err(|_| Error::Key)??;
        signature
            .ok_or(Error::Key)?
            .try_into()
            .map_err(|_| Error::Key)
    }
    async fn wrap(&self, _: &str, _: &[u8; 32]) -> Result<String> {
        Err(Error::Key)
    }
    async fn unwrap(&self, alias: &str, sealed: &str) -> Result<Zeroizing<Vec<u8>>> {
        let (alias, sealed) = (alias.to_owned(), sealed.to_owned());
        tokio::task::spawn_blocking(move || {
            call(|env, class| {
                let alias = env.new_string(alias)?;
                let sealed = env.new_string(sealed)?;
                let seed = JByteArray::from(
                    env.call_static_method(
                        class,
                        "unwrap",
                        "(Ljava/lang/String;Ljava/lang/String;)[B",
                        &[JValue::Object(&alias), JValue::Object(&sealed)],
                    )?
                    .l()?,
                );
                if seed.is_null() {
                    return Ok(None);
                }
                let copy = Zeroizing::new(env.convert_byte_array(&seed)?);
                let zeros = vec![0_i8; copy.len()];
                env.set_byte_array_region(&seed, 0, &zeros)?;
                Ok(Some(copy))
            })
        })
        .await
        .map_err(|_| Error::Key)??
        .ok_or(Error::Key)
    }
    async fn delete(&self, _: &str) -> Result<()> {
        Err(Error::Key)
    }
}

/// `AlertWatcher.start(dataDir, cleared)`: `dataDir` is Android's for the app,
/// where Tauri keeps the app's own files too; `cleared` changes when Android
/// clears the app's notifications.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_ophio_device_AlertWatcher_start<'local>(
    mut env: JNIEnv<'local>,
    class: JClass<'local>,
    data_dir: JString<'local>,
    cleared: JString<'local>,
) -> jboolean {
    let started = (|| -> Result<()> {
        let native = |e: jni::errors::Error| Error::Native(e.to_string());
        attach(&mut env, &class)?;
        let dir: String = env.get_string(&data_dir).map_err(native)?.into();
        let cleared: String = env.get_string(&cleared).map_err(native)?.into();
        super::start(
            &PathBuf::from(dir),
            &cleared,
            Arc::new(Android),
            Arc::new(Keys),
        )
    })();
    match started {
        Ok(()) => JNI_TRUE,
        Err(error) => {
            tracing::warn!(%error, "alerts: the watcher did not start");
            JNI_FALSE
        }
    }
}

/// `AlertWatcher.answer(dataDir, reply, text)`: sends the person's answer
/// from a question's notification and waits for the computer to take it.
/// Null once taken; otherwise why not, in the person's words. Kotlin calls
/// it off the main thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_ophio_device_AlertWatcher_answer<'local>(
    mut env: JNIEnv<'local>,
    class: JClass<'local>,
    data_dir: JString<'local>,
    reply: JString<'local>,
    text: JString<'local>,
) -> jstring {
    let sent = (|| -> Result<()> {
        let native = |e: jni::errors::Error| Error::Native(e.to_string());
        attach(&mut env, &class)?;
        let dir: String = env.get_string(&data_dir).map_err(native)?.into();
        let reply: String = env.get_string(&reply).map_err(native)?.into();
        let reply: super::Reply =
            serde_json::from_str(&reply).map_err(|_| Error::Invalid("Invalid answer."))?;
        let text: String = env.get_string(&text).map_err(native)?.into();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Error::Native("The phone could not send the answer.".into()))?;
        runtime.block_on(super::answer(&PathBuf::from(dir), &Keys, &reply, &text))
    })();
    let Err(error) = sent else {
        return std::ptr::null_mut();
    };
    tracing::warn!(%error, "alerts: an answer was not sent");
    // Should even this fail, Kotlin sees the exception and says it wasn't sent.
    env.new_string(super::unsent(&error))
        .map(|why| why.into_raw())
        .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_ophio_device_AlertWatcher_stop(_: JNIEnv, _: JClass) {
    super::stop();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_ophio_device_AlertWatcher_nudge(_: JNIEnv, _: JClass) {
    super::nudge();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_app_ophio_device_AlertWatcher_retry(_: JNIEnv, _: JClass) {
    super::retry();
}

/// Whether the watcher runs, for the app's screen to ask Android's side.
#[unsafe(no_mangle)]
pub extern "system" fn Java_app_ophio_device_AlertWatcher_running(
    _: JNIEnv,
    _: JClass,
) -> jboolean {
    if super::running() {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}
