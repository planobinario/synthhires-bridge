/// JNI entry points for Android.
/// Compiled only when `cfg(target_os = "android")` — cargo-ndk builds.
/// Desktop binary ignores this module (empty stub on non-Android targets).

#[cfg(target_os = "android")]
mod imp {
    use crate::{CapabilityGate, DeviceFingerprint, WsClient};
    use std::ffi::{c_char, c_int, CStr};
    use tokio::runtime::Runtime;

    #[allow(improper_ctypes)]
    extern "system" {
        fn GetStringUTFChars(env: *mut c_void, str: *mut c_void, is_copy: *mut u8)
            -> *const c_char;
        fn ReleaseStringUTFChars(env: *mut c_void, str: *mut c_void, utf: *const c_char);
    }

    fn get_string(env: *mut c_void, jstr: *mut c_void) -> String {
        if jstr.is_null() {
            return String::new();
        }
        unsafe {
            let raw = GetStringUTFChars(env, jstr, std::ptr::null_mut());
            if raw.is_null() {
                return String::new();
            }
            let s = CStr::from_ptr(raw).to_string_lossy().into_owned();
            ReleaseStringUTFChars(env, jstr, raw);
            s
        }
    }

    #[no_mangle]
    pub extern "system" fn Java_com_synthhires_bridge_DaemonCore_runBridge(
        env: *mut c_void,
        _class: *mut c_void,
        token: *mut c_void,
        device_id: *mut c_void,
        backend_url: *mut c_void,
    ) {
        let token = get_string(env, token);
        let device_id = get_string(env, device_id);
        let backend_url = get_string(env, backend_url);

        let fingerprint = DeviceFingerprint::compute();
        let gate = CapabilityGate::new(Default::default());
        let chat_store = std::sync::Arc::new(
            crate::ChatStore::open(&crate::ChatStore::default_path()).unwrap_or_else(|e| {
                tracing::error!("[chat-store] open failed, falling back to memory: {e}");
                crate::ChatStore::open(std::path::Path::new(":memory:"))
                    .expect("in-memory chat store")
            }),
        );
        let client = WsClient::new(
            backend_url,
            token,
            device_id,
            fingerprint,
            "mobile",
            "Android Bridge",
            gate,
            chat_store,
        );

        let rt = Runtime::new().expect("tokio runtime for Android bridge");
        rt.block_on(async {
            if let Err(e) = client.run().await {
                tracing::error!("bridge loop exited: {e}");
            }
        });
    }
}
