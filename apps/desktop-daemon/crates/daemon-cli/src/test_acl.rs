use interprocess::local_socket::ListenerOptions;
use interprocess::os::windows::local_socket::ListenerOptionsExt;
use std::ptr;
use windows_sys::Win32::Security::Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW;
use windows_sys::Win32::Security::SDDL_REVISION_1;
use std::os::windows::ffi::OsStrExt;

fn main() {
    let mut options = ListenerOptions::new().name(r"\\.\pipe\synthhires-test".into());
    let sddl: Vec<u16> = std::ffi::OsStr::new("D:(A;;GA;;;OW)").encode_wide().chain(std::iter::once(0)).collect();
    let mut sd: *mut std::ffi::c_void = ptr::null_mut();
    
    let res = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            ptr::null_mut(),
        )
    };
    
    if res == 0 {
        panic!("Failed to create SD");
    }
    
    options = options.security_descriptor(sd);
    println!("Compiled successfully!");
}
