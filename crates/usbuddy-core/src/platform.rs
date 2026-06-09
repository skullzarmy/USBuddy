use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PlatformInfo {
    pub os: String,
    pub arch: String,
    pub family: String,
}

pub fn detect_platform() -> PlatformInfo {
    PlatformInfo {
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        family: std::env::consts::FAMILY.to_owned(),
    }
}
