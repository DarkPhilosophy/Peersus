//! Initialize the platform trust manager before activity or service networking.
//! The verifier uses JNI 0.22 while the existing bridge uses JNI 0.21. Only borrowed
//! pointers from this same Java native frame cross that version boundary.
#![allow(unsafe_code)]

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_peersus_app_PluginBridge_initializeTls<'local>(
    env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    context: jni::objects::JObject<'local>,
) {
    // SAFETY: the JVM supplied a non-null environment for this thread and context
    // is a live local reference in this native frame. Neither pointer escapes it;
    // init_with_env creates the verifier's own global references.
    let mut unowned = unsafe { jni_verifier::EnvUnowned::from_raw(env.get_raw().cast()) };
    unowned
        .with_env(|env| {
            // SAFETY: the borrowed context belongs to the same native frame/thread.
            let context =
                unsafe { jni_verifier::objects::JObject::from_raw(env, context.as_raw().cast()) };
            rustls_platform_verifier::android::init_with_env(env, context)
        })
        // Fail closed before networking if initialization fails. The JNI wrapper
        // catches unwinds and reports a Java exception rather than unwinding FFI.
        .resolve::<jni_verifier::errors::ThrowRuntimeExAndDefault>();
}
