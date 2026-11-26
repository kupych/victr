//! Application location, generated retro woodgrain CRT icon, and Linux desktop menu entry.

use std::path::PathBuf;

/// The running program's file.
pub fn exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// The project's app folder (the program lives in target/<debug|release>/).
pub fn app_dir() -> Option<PathBuf> {
    let exe = exe_path()?;
    let dir = exe.parent()?.parent()?.parent()?.to_path_buf();
    dir.join("victr.sh").exists().then_some(dir)
}

/// The double-clickable launcher in the app folder, if there is one.
#[allow(dead_code)]
pub fn launcher_path() -> Option<PathBuf> {
    app_dir().map(|d| d.join("victr.sh"))
}

/// Procedural retro icon: 1970s walnut woodgrain CRT receiver with brushed-metal
/// control strip, rotary tuning dials, glowing power LED, and RGB phosphor tube.
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
            let aa = 2.2 / s;
            let cab = rbox(x, y, 0.94, 0.80, 0.16);
            let trim = rbox(x, y, 0.83, 0.69, 0.11);
            // Screen offset slightly left to leave room for the classic metal tuner panel on the right
            let sx = x + 0.12;
            let scr_bezel = rbox(sx, y, 0.63, 0.59, 0.12);
            let scr = rbox(sx * (1.0 - 0.05 * y * y), y * (1.0 - 0.05 * sx * sx), 0.56, 0.52, 0.11);
            let metal = rbox(x - 0.65, y, 0.13, 0.59, 0.04);

            let mut c = [0.0f32; 4];
            if cab < aa {
                let a = ((aa - cab) / (2.0 * aa)).clamp(0.0, 1.0);
                // Warm faux walnut woodgrain
                let grain = 0.5
                    + 0.35 * (y * 34.0 + (x * 4.5).sin() * 1.4).sin()
                    + 0.15 * (y * 82.0 - x * 2.5).sin();
                let shade = 0.82 - 0.24 * y;
                let r = (0.14 + 0.14 * grain) * shade;
                let g = (0.065 + 0.065 * grain) * shade;
                let b = (0.028 + 0.025 * grain) * shade;
                c = [r, g, b, a];
            }
            // Thin champagne-chrome trim pinstripe
            if trim.abs() < 0.025 + aa && cab < 0.0 {
                let a = ((0.025 + aa - trim.abs()) / (2.0 * aa)).clamp(0.0, 1.0);
                let chrome = 0.52 - 0.18 * y;
                let col = [chrome, chrome * 0.94, chrome * 0.82];
                for ch in 0..3 {
                    c[ch] = c[ch] * (1.0 - a) + col[ch] * a;
                }
            }
            // Dark recessed charcoal inner bezel around tube
            if scr_bezel < aa {
                let a = ((aa - scr_bezel) / (2.0 * aa)).clamp(0.0, 1.0);
                let col = [0.032 - 0.01 * y, 0.030 - 0.01 * y, 0.028 - 0.01 * y];
                for ch in 0..3 {
                    c[ch] = c[ch] * (1.0 - a) + col[ch] * a;
                }
            }
            // Brushed aluminum control strip on the right side
            if metal < aa {
                let a = ((aa - metal) / (2.0 * aa)).clamp(0.0, 1.0);
                let brush = 0.5 + 0.5 * (y * 120.0).sin();
                let m = 0.24 + 0.06 * brush - 0.07 * y;
                let col = [m, m * 0.96, m * 0.90];
                for ch in 0..3 {
                    c[ch] = c[ch] * (1.0 - a) + col[ch] * a;
                }
                // Two metallic rotary VHF/UHF tuning dials
                for &ky in &[-0.28f32, 0.02f32] {
                    let d = ((x - 0.65).powi(2) + (y - ky).powi(2)).sqrt();
                    if d < 0.095 + aa {
                        let ka = ((0.095 + aa - d) / (2.0 * aa)).clamp(0.0, 1.0);
                        let ring = if d > 0.068 { 0.55 } else { 0.10 };
                        let col = [ring, ring * 0.95, ring * 0.88];
                        for ch in 0..3 {
                            c[ch] = c[ch] * (1.0 - ka) + col[ch] * ka;
                        }
                    }
                }
                // Glowing amber-red power indicator lamp
                let ld = ((x - 0.65).powi(2) + (y - 0.36).powi(2)).sqrt();
                if ld < 0.055 + aa {
                    let la = ((0.055 + aa - ld) / (2.0 * aa)).clamp(0.0, 1.0);
                    let glow = (1.0 - ld / 0.06).clamp(0.0, 1.0);
                    let col = [0.95 * glow + 0.15, 0.30 * glow, 0.06 * glow];
                    for ch in 0..3 {
                        c[ch] = c[ch] * (1.0 - la) + col[ch] * la;
                    }
                }
            }
            // Glowing CRT glass screen with retro phosphor stripes + amber/cyan scanlines
            if scr < aa {
                let a = ((aa - scr) / (2.0 * aa)).clamp(0.0, 1.0);
                let stripe = ((sx * 22.0).rem_euclid(3.0)) as usize;
                let scan = 0.72 + 0.28 * (y * 46.0).sin();
                let glow = (1.0 - (sx * sx * 1.1 + y * y * 1.2)).clamp(0.0, 1.0);
                let base = [1.0, 0.66, 0.26];
                let mut col = [0.05f32, 0.07, 0.09];
                let k = (0.32 + 0.95 * glow) * scan;
                col[stripe] += base[stripe] * k;
                col[(stripe + 1) % 3] += 0.24 * base[(stripe + 1) % 3] * k;
                // Curved glass specular highlight
                let hl = ((sx + 0.24).powi(2) + (y + 0.24).powi(2)).sqrt();
                let spec = (-hl * hl / 0.08).exp() * 0.28;
                for (ch, v) in col.iter().enumerate() {
                    c[ch] = c[ch] * (1.0 - a) + (v + spec).min(1.0) * a;
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

#[cfg(any(target_os = "linux", test))]
fn desktop_entry(exe: &std::path::Path, icon: &std::path::Path) -> String {
    let q = |p: &std::path::Path| p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "[Desktop Entry]\nType=Application\nName=ViCTR\nGenericName=Virtual CRT Television Receiver\n\
         Comment=Physically based CRT television simulator\nExec=\"{}\" %f\nIcon={}\nTerminal=false\nStartupWMClass=victr\n\
         Categories=AudioVideo;Video;Player;\nMimeType=video/mp4;video/x-matroska;video/webm;video/x-msvideo;video/quicktime;\n",
        q(exe),
        icon.to_string_lossy()
    )
}

/// Adds ViCTR to the desktop's apps menu. Returns a message for the user.
#[cfg(target_os = "linux")]
pub fn install_menu_entry() -> Result<String, String> {
    let exe = launcher_path().or_else(exe_path).ok_or("Couldn't find where this program is.")?;
    let icon = app_dir().map(|d| d.join("victr.png")).unwrap_or_else(|| exe.with_file_name("victr.png"));
    if !icon.exists() {
        let rgba = icon_rgba(256);
        image::save_buffer(&icon, &rgba, 256, 256, image::ColorType::Rgba8).map_err(|e| format!("Couldn't save the icon: {e}"))?;
    }
    let entry = desktop_entry(&exe, &icon);
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("No home folder found.")?;
    let apps = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".local/share")).join("applications");
    let file = apps.join("victr.desktop");
    let direct = std::fs::create_dir_all(&apps).and_then(|_| std::fs::write(&file, &entry));
    if direct.is_err() && std::path::Path::new("/.flatpak-info").exists() {
        // Inside a Flatpak sandbox the home folder may be read-only,
        // so ask the host system to write the menu entry.
        use std::io::Write;
        let script = "mkdir -p \"$1\" && cat > \"$1/victr.desktop\"";
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
    Ok(format!("Added \"ViCTR\" to your apps menu ({}).", file.display()))
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
        let out = std::env::temp_dir().join(format!("victr-icon-test-{}.png", std::process::id()));
        image::save_buffer(&out, &px, 256, 256, image::ColorType::Rgba8).unwrap();
        let _ = std::fs::remove_file(&out);
        let e = desktop_entry(std::path::Path::new("/home/me/victr/victr"), std::path::Path::new("/x/icon.png"));
        assert!(e.contains("Exec=\"/home/me/victr/victr\" %f"));
        assert!(e.contains("StartupWMClass=victr"));
    }
}
