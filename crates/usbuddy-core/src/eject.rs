//! Detached OS-level eject of the USBuddy drive.
//!
//! The runtime binary itself lives on the drive, so the volume cannot be
//! ejected while the process is alive. The trick: spawn a host-resident
//! shell that sleeps briefly, then retries the platform eject command until
//! the volume lets go — and have the runtime exit immediately afterwards.
//! The orphaned child finishes the job once nothing on the drive is running.

use std::io;
use std::path::Path;
use std::process::Command;

/// Spawns a detached helper process that ejects the volume at `mount` after
/// a short delay. Returns once the helper is spawned — the actual eject
/// happens after the calling process exits. The helper's working directory
/// is set off-drive so it never pins the volume itself.
pub fn spawn_detached_eject(mount: &Path) -> io::Result<()> {
    let mount_str = mount.to_string_lossy().into_owned();

    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "sleep 1; for i in $(seq 1 15); do \
               diskutil eject \"{mount_str}\" >/dev/null 2>&1 && exit 0; sleep 1; \
             done; exit 1"
        );
        Command::new("/bin/sh")
            .args(["-c", &script])
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Unmount first (retrying while the runtime finishes exiting), then
        // power the device off via udisks so the stick is safe to unplug.
        // `eject` is the fallback for systems without udisks.
        let script = format!(
            "sleep 1; dev=$(findmnt -rno SOURCE \"{mount_str}\" 2>/dev/null); \
             for i in $(seq 1 15); do \
               umount \"{mount_str}\" >/dev/null 2>&1 && break; sleep 1; \
             done; \
             if [ -n \"$dev\" ]; then \
               udisksctl power-off -b \"$dev\" >/dev/null 2>&1 || eject \"$dev\" >/dev/null 2>&1; \
             fi"
        );
        Command::new("/bin/sh")
            .args(["-c", &script])
            .current_dir("/")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: survive parent exit,
        // no console window.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        // Shell.Application's InvokeVerb('Eject') is the only stable
        // no-extra-tooling eject on Windows; it needs the bare drive letter.
        let drive_letter = mount_str
            .get(..2)
            .filter(|s| s.ends_with(':'))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("cannot derive drive letter from '{mount_str}'"),
                )
            })?
            .to_string();
        let script = format!(
            "Start-Sleep 1; $sh = New-Object -ComObject Shell.Application; \
             for ($i = 0; $i -lt 15; $i++) {{ \
               $sh.Namespace(17).ParseName('{drive_letter}').InvokeVerb('Eject'); \
               Start-Sleep 1; \
               if (-not (Test-Path '{drive_letter}\\')) {{ exit 0 }} \
             }}"
        );
        Command::new("powershell")
            .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command", &script])
            .current_dir("C:\\")
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        Ok(())
    }
}
