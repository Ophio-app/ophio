// SPDX-License-Identifier: Apache-2.0
//! Private inherited CRT descriptors 3/4, restricted to two child pipe handles.
#![allow(unsafe_code)]
use std::{
    ffi::OsStr,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    sync::Arc,
};
use voice_control::Error;
use windows::{
    Win32::{
        Foundation::*,
        Security::SECURITY_ATTRIBUTES,
        System::{Pipes::CreatePipe, Threading::*},
    },
    core::{PCWSTR, PWSTR},
};
fn failure() -> Error {
    Error::Unavailable("The separate browser’s private pipes could not start".into())
}
fn quote(value: &OsStr) -> Vec<u16> {
    let mut result = vec![b'"' as u16];
    let mut slashes = 0;
    for c in value.encode_wide() {
        if c == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        result.extend(std::iter::repeat_n(
            b'\\' as u16,
            if c == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        result.push(c);
    }
    result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    result.push(b'"' as u16);
    result
}
#[derive(Debug)]
pub struct Process(Arc<OwnedHandle>);
impl Process {
    pub async fn finish(&self) {
        let process = self.0.clone();
        let _ = tokio::task::spawn_blocking(move || {
            // SAFETY: the owned process handle stays alive throughout both waits and termination.
            unsafe {
                let h = HANDLE(process.as_raw_handle());
                if WaitForSingleObject(h, 5000) == WAIT_TIMEOUT {
                    let _ = TerminateProcess(h, 0);
                    let _ = WaitForSingleObject(h, 5000);
                }
            }
        })
        .await;
    }
}
// Anonymous Windows pipes are synchronous. Dedicated OS threads keep blocked pipe
// reads out of Tokio's blocking pool, so leaving the browser open cannot hold up
// runtime shutdown. Queues are bounded and acknowledgements still come from CDP.
#[derive(Debug)]
pub struct PipeInput(tokio::sync::mpsc::Sender<Vec<u8>>);
pub struct PipeOutput {
    receiver: tokio::sync::mpsc::Receiver<Vec<u8>>,
    buffered: std::io::Cursor<Vec<u8>>,
}
impl std::fmt::Debug for PipeOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrowserPipeOutput")
    }
}
impl tokio::io::AsyncWrite for PipeInput {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(
            self.0
                .try_send(bytes.to_vec())
                .map(|()| bytes.len())
                .map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "Browser pipe queue unavailable",
                    )
                }),
        )
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
impl tokio::io::AsyncRead for PipeOutput {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        out: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if out.remaining() == 0 {
            return std::task::Poll::Ready(Ok(()));
        }
        if self.buffered.position() as usize == self.buffered.get_ref().len() {
            match self.receiver.poll_recv(cx) {
                std::task::Poll::Pending => return std::task::Poll::Pending,
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(Ok(())),
                std::task::Poll::Ready(Some(bytes)) => self.buffered = std::io::Cursor::new(bytes),
            }
        }
        let at = self.buffered.position() as usize;
        let n = out.remaining().min(self.buffered.get_ref().len() - at);
        out.put_slice(&self.buffered.get_ref()[at..at + n]);
        self.buffered.set_position((at + n) as u64);
        std::task::Poll::Ready(Ok(()))
    }
}
fn threads(input: std::fs::File, output: std::fs::File) -> Result<(PipeInput, PipeOutput), Error> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    std::thread::Builder::new()
        .name("browser-pipe-write".into())
        .spawn(move || {
            use std::io::Write;
            let mut input = input;
            while let Some(bytes) = rx.blocking_recv() {
                if input.write_all(&bytes).is_err() {
                    break;
                }
            }
        })
        .map_err(|_| failure())?;
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    std::thread::Builder::new()
        .name("browser-pipe-read".into())
        .spawn(move || {
            use std::io::Read;
            let mut output = output;
            let mut bytes = [0u8; 32768];
            loop {
                match output.read(&mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sender.blocking_send(bytes[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .map_err(|_| failure())?;
    Ok((
        PipeInput(tx),
        PipeOutput {
            receiver,
            buffered: std::io::Cursor::new(Vec::new()),
        },
    ))
}
pub fn launch(
    executable: &Path,
    arguments: &[std::ffi::OsString],
) -> Result<(PipeInput, PipeOutput, Process), Error> {
    // SAFETY: all inherited handles are owned until CreateProcess completes. The attribute and CRT buffers remain allocated and correctly aligned throughout the call. Parent ends are explicitly non-inheritable.
    unsafe {
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        fn pair(sa: &SECURITY_ATTRIBUTES) -> Result<(OwnedHandle, OwnedHandle), Error> {
            let mut r = HANDLE::default();
            let mut w = HANDLE::default();
            // SAFETY: output pointers are valid; ownership is taken immediately after successful creation.
            unsafe {
                CreatePipe(&mut r, &mut w, Some(sa), 0).map_err(|_| failure())?;
                Ok((
                    OwnedHandle::from_raw_handle(r.0),
                    OwnedHandle::from_raw_handle(w.0),
                ))
            }
        }
        let (child_read, parent_write) = pair(&sa)?;
        let (parent_read, child_write) = pair(&sa)?;
        for h in [&parent_write, &parent_read] {
            SetHandleInformation(
                HANDLE(h.as_raw_handle()),
                HANDLE_FLAG_INHERIT.0,
                HANDLE_FLAGS(0),
            )
            .map_err(|_| failure())?;
        }
        let inherited = [
            HANDLE(child_read.as_raw_handle()),
            HANDLE(child_write.as_raw_handle()),
        ];
        let mut size = 0;
        let _ = InitializeProcThreadAttributeList(None, 1, Some(0), &mut size);
        let mut attributes = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(attributes.as_mut_ptr().cast());
        InitializeProcThreadAttributeList(Some(list), 1, Some(0), &mut size)
            .map_err(|_| failure())?;
        struct Attributes(LPPROC_THREAD_ATTRIBUTE_LIST);
        impl Drop for Attributes {
            fn drop(&mut self) {
                // SAFETY: initialized list backed by the still-live aligned allocation.
                unsafe { DeleteProcThreadAttributeList(self.0) }
            }
        }
        let _cleanup = Attributes(list);
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            Some(inherited.as_ptr().cast()),
            std::mem::size_of_val(&inherited),
            None,
            None,
        )
        .map_err(|_| failure())?;
        // MSVCRT startup descriptor table: count, flags[count], native handles[count].
        let mut descriptors = Vec::new();
        descriptors.extend_from_slice(&5i32.to_ne_bytes());
        descriptors.extend_from_slice(&[0, 0, 0, 9, 9]);
        for raw in [
            usize::MAX,
            usize::MAX,
            usize::MAX,
            child_read.as_raw_handle() as usize,
            child_write.as_raw_handle() as usize,
        ] {
            descriptors.extend_from_slice(&raw.to_ne_bytes());
        }
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.cbReserved2 = descriptors.len() as u16;
        startup.StartupInfo.lpReserved2 = descriptors.as_mut_ptr();
        startup.lpAttributeList = list;
        let exe: Vec<u16> = executable
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut command = quote(executable.as_os_str());
        for arg in arguments {
            command.push(b' ' as u16);
            command.extend(quote(arg));
        }
        command.push(0);
        let mut process = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR(exe.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            true,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            None,
            &startup.StartupInfo,
            &mut process,
        )
        .map_err(|_| failure())?;
        let process_handle = OwnedHandle::from_raw_handle(process.hProcess.0);
        let _thread = OwnedHandle::from_raw_handle(process.hThread.0);
        let (input, output) = threads(
            std::fs::File::from(parent_write),
            std::fs::File::from(parent_read),
        )?;
        Ok((input, output, Process(Arc::new(process_handle))))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arguments_preserve_quotes_and_slashes() {
        assert_eq!(
            String::from_utf16(&quote(OsStr::new("a b\\"))).unwrap(),
            "\"a b\\\\\""
        );
        assert_eq!(
            String::from_utf16(&quote(OsStr::new("x\"y"))).unwrap(),
            "\"x\\\"y\""
        );
    }
}
