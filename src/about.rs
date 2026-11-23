//! Application location, generated window icon, and Linux desktop menu entry.

use std::path::PathBuf;

/// The running program's file.
pub fn exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// The project's app folder (the program lives in app/target/<debug|release>/).
pub fn app_dir() -> Option<PathBuf> {
    let exe = exe_path()?;
    let dir = exe.parent()?.parent()?.parent()?.to_path_buf();
    dir.join("phosphor-lab.sh").exists().then_some(dir)
}

/// The double-clickable launcher in the app folder, if there is one.
pub fn launcher_path() -> Option<PathBuf> {
    app_dir().map(|d| d.join("phosphor-lab.sh"))
}

/// A small original icon: a dark TV cabinet with a glowing RGB-striped screen.
pub fn icon_rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let rbox = |x: f32, y: f32, hw: f32, hh: f32, r: f32| -> f32 {
        let qx = (x.abs() - hw + r).max(0.0);
        let qy = (y.abs() - hh + r).max(0.0);
        (qx * qx + qy * qy).sqrt() + (x.abs() - hw + r).max(y.abs() - hh + r).min(0.0) - r
    };
    for j in 0..size {
        for i in 0..size {
            let x = (i as f32 + 0.5) / s * 2.0 - 1.0;
            let y = (j as f32 + 0.5) / s * 2.0 - 1.0;
            let aa = 2.0 / s;
            let cab = rbox(x, y + 0.02, 0.94, 0.80, 0.22);
            let scr = rbox(x * (1.0 - 0.06 * y * y), y * (1.0 - 0.06 * x * x) + 0.02, 0.74, 0.60, 0.16);
            let mut c = [0.0f32; 4];
            if cab < aa {
                let a = ((aa - cab) / (2.0 * aa)).clamp(0.0, 1.0);
                let shade = 0.035 - 0.015 * y;
                c = [shade, shade * 0.95, shade * 0.9, a];
            }
            if scr < aa {
                let a = ((aa - scr) / (2.0 * aa)).clamp(0.0, 1.0);
                // phosphor stripes + scanlines + centre glow
                let stripe = ((x * 18.0).rem_euclid(3.0)) as i32;
                let scan = 0.75 + 0.25 * (y * 42.0).sin();
                let glow = (1.0 - (x * x * 0.8 + y * y)).clamp(0.0, 1.0);
                let base = [1.0, 0.62, 0.28];
                let mut col = [0.08f32, 0.08, 0.1];
                let k = (0.35 + 0.9 * glow) * scan;
                col[stripe as usize] += base[stripe as usize] * k;
                col[(stripe as usize + 1) % 3] += 0.25 * base[(stripe as usize + 1) % 3] * k;
                for (ch, v) in col.iter().enumerate() {
                    c[ch] = c[ch] * (1.0 - a) + v.min(1.0) * a;
                }
                c[3] = c[3].max(a);
            }
            let o = ((j * size + i) * 4) as usize;
            for ch in 0..3 {
                px[o + ch] = (c[ch].clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0) as u8;
            }
            px[o + 3] = (c[3].clamp(0.0, 1.0) * 255.0) as u8;
        }
    }
    px
}

fn desktop_entry(exe: &std::path::Path, icon: &std::path::Path) -> String {
    let q = |p: &std::path::Path| p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "[Desktop Entry]\nType=Application\nName=Phosphor Lab\nGenericName=CRT simulator\n\
         Comment=Physically based CRT television simulator\nExec=\"{}\" %f\nIcon={}\nTerminal=false\nStartupWMClass=phosphor-lab\n\
         Categories=AudioVideo;Video;Player;\nMimeType=video/mp4;video/x-matroska;video/webm;video/x-msvideo;video/quicktime;\n",
        q(exe),
        icon.to_string_lossy()
    )
}

/// Adds Phosphor Lab to the desktop's apps menu. Returns a message for the person.
#[cfg(target_os = "linux")]
pub fn install_menu_entry() -> Result<String, String> {
    // prefer the launcher: it starts the optimized build and keeps it up to date
    let exe = launcher_path().or_else(exe_path).ok_or("Couldn't find where this program is.")?;
    // the icon sits in the project folder
    let icon = app_dir().map(|d| d.join("phosphor-lab.png")).unwrap_or_else(|| exe.with_file_name("phosphor-lab.png"));
    if !icon.exists() {
        let rgba = icon_rgba(256);
        image::save_buffer(&icon, &rgba, 256, 256, image::ColorType::Rgba8).map_err(|e| format!("Couldn't save the icon: {e}"))?;
    }
    let entry = desktop_entry(&exe, &icon);
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("No home folder found.")?;
    let apps = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("applications");
    let file = apps.join("phosphor-lab.desktop");
    let direct = std::fs::create_dir_all(&apps).and_then(|_| std::fs::write(&file, &entry));
    if direct.is_err() && std::path::Path::new("/.flatpak-info").exists() {
        // Inside a Flatpak sandbox the home folder may be read-only,
        // so ask the host system to write the menu entry.
        use std::io::Write;
        let script = "mkdir -p \"$1\" && cat > \"$1/phosphor-lab.desktop\"";
        let mut child = std::process::Command::new("flatpak-spawn")
            .args(["--host", "sh", "-c", script, "sh"])
            .arg(&apps)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Couldn't add the menu entry: {e}"))?;
        child.stdin.take().unwrap().write_all(entry.as_bytes()).map_err(|e| e.to_string())?;
        let ok = child.wait().map(|s| s.success()).unwrap_or(false);
        if !ok {
            return Err("Couldn't add the menu entry.".into());
        }
    } else if let Err(e) = direct {
        return Err(format!("Couldn't add the menu entry: {e}"));
    }
    Ok(format!("Added \"Phosphor Lab\" to your apps menu ({}).", file.display()))
}

#[cfg(not(target_os = "linux"))]
pub fn install_menu_entry() -> Result<String, String> {
    Err("Adding a menu entry is only available on Linux for now; you can make a shortcut to the program shown above.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_and_menu_entry() {
        let px = icon_rgba(256);
        assert_eq!(px.len(), 256 * 256 * 4);
        let out = std::env::temp_dir().join(format!("phosphor-icon-test-{}.png", std::process::id()));
        image::save_buffer(&out, &px, 256, 256, image::ColorType::Rgba8).unwrap();
        let _ = std::fs::remove_file(&out);
        let e = desktop_entry(std::path::Path::new("/home/me/phosphor-lab/crt_sim"), std::path::Path::new("/x/icon.png"));
        assert!(e.contains("Exec=\"/home/me/phosphor-lab/crt_sim\" %f"));
        assert!(e.contains("StartupWMClass=phosphor-lab"));
    }
}
