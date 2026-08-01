fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        res.set("FileDescription", "SynthHires Desktop Bridge");
        res.set("ProductName", "SynthHires");
        res.set("OriginalFilename", "synthhires-bridge.exe");
        res.compile().unwrap();
    }
}
