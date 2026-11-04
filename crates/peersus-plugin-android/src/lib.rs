//! Only interpreter + JNI transport, with no UI, networking identity, or native loader.
#![cfg(target_os = "android")]
#![allow(unsafe_code)]
use jni::{
    JNIEnv,
    objects::{JObject, JString},
    sys::jint,
};
use peersus_plugin_runtime::{MAX_MODULE_BYTES, socket::SocketTransport};
use std::{
    fs::File,
    io::{self, Read},
    os::{fd::FromRawFd, unix::net::UnixStream},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

/// SAFETY: Java detaches each descriptor exactly once before this call; Rust owns both
/// even if validation fails. The module runs only in the manifest's isolated service.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_WasmPluginService_nativeRun(
    mut env: JNIEnv<'_>,
    object: JObject<'_>,
    module: jint,
    control: jint,
    token: JString<'_>,
) -> jint {
    if module < 0 || control < 0 || module == control {
        return 1;
    }
    // SAFETY: ownership transferred from ParcelFileDescriptor.detachFd, no duplicate owners.
    let module = unsafe { File::from_raw_fd(module) };
    let control = unsafe { UnixStream::from_raw_fd(control) };
    let mut failed_stage = 2;
    let outcome = catch_unwind(AssertUnwindSafe(|| -> Option<()> {
        if !env
            .call_method(&object, "isWorkerProcess", "()Z", &[])
            .ok()?
            .z()
            .ok()?
        {
            return None;
        }
        failed_stage = 3;
        let token: String = env.get_string(&token).ok()?.into();
        if token.len() != 64
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return None;
        }
        failed_stage = 4;
        let mut bytes = Vec::new();
        module
            .take((MAX_MODULE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_MODULE_BYTES {
            return None;
        }
        failed_stage = 5;
        let vm = env.get_java_vm().ok()?;
        let service = env.new_global_ref(&object).ok()?;
        let platform = Arc::new((vm, service));
        failed_stage = 6;
        let mut transport = SocketTransport::from_connected(control, token, move || {
            let (vm, service) = platform.as_ref();
            let mut env = vm
                .attach_current_thread()
                .map_err(|_| io::Error::other("data descriptor unavailable"))?;
            let result = env
                .call_method(service.as_obj(), "openData", "()I", &[])
                .and_then(|result| result.i());
            let fd = match result {
                Ok(fd) if fd >= 0 => fd,
                _ => {
                    let _ = env.exception_clear();
                    return Err(io::Error::other("data descriptor unavailable"));
                }
            };
            // SAFETY: openData detaches one newly received Binder descriptor to this worker.
            Ok(unsafe { UnixStream::from_raw_fd(fd) })
        })
        .map_err(|error| {
            failed_stage = 600 + error.raw_os_error().unwrap_or(99).clamp(0, 255);
        })
        .ok()?;
        failed_stage = 7;
        peersus_plugin_runtime::run(&bytes, &mut transport).ok()
    }));
    match outcome {
        Ok(Some(())) => 0,
        Ok(None) => failed_stage,
        Err(_) => 8,
    }
}
