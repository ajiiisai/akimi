use std::env;
use std::path::PathBuf;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

pub(crate) fn prepare_display() -> Result<(), String> {
    // WAYLAND_SOCKET takes precedence over WAYLAND_DISPLAY in wayland-client.
    // A stale value can make a perfectly valid niri socket look unavailable.
    if env::var_os("WAYLAND_SOCKET").is_some_and(|value| {
        value
            .to_str()
            .and_then(|value| value.parse::<i32>().ok())
            .is_none()
    }) {
        env::remove_var("WAYLAND_SOCKET");
    }

    let wayland = non_empty_var("WAYLAND_DISPLAY");
    let x11 = non_empty_var("DISPLAY").is_some();
    let wayland_session = non_empty_var("XDG_SESSION_TYPE")
        .is_some_and(|session| session.eq_ignore_ascii_case("wayland"));

    if env::var("AKIMI_DISPLAY_BACKEND").is_ok_and(|value| value.eq_ignore_ascii_case("x11")) {
        if x11 {
            env::remove_var("WAYLAND_DISPLAY");
            return Ok(());
        }
        return Err("AKIMI_DISPLAY_BACKEND=x11 was requested, but DISPLAY is not set".into());
    }

    if let Some(display) = wayland {
        // Desktop launchers can leave WAYLAND_DISPLAY set when the current
        // session is X11. GPUI prefers Wayland whenever the variable exists.
        if x11 && !wayland_session {
            env::remove_var("WAYLAND_DISPLAY");
            return Ok(());
        }

        let socket = wayland_socket_path(&display);
        if wayland_socket_is_reachable(&socket) {
            return Ok(());
        }

        if x11 {
            eprintln!(
                "Akimi: Wayland socket '{}' is unavailable; falling back to X11.",
                socket.display()
            );
            env::remove_var("WAYLAND_DISPLAY");
            return Ok(());
        }

        return Err(format!(
            "Wayland is selected by WAYLAND_DISPLAY, but '{}' is unavailable and DISPLAY is not set",
            socket.display()
        ));
    }

    if x11 {
        return Ok(());
    }

    Err("no graphical display was found; start a Wayland/X11 session or set WAYLAND_DISPLAY/DISPLAY".into())
}

fn non_empty_var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn wayland_socket_path(display: &str) -> PathBuf {
    let display = PathBuf::from(display);
    if display.is_absolute() {
        return display;
    }

    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(display)
}

#[cfg(unix)]
fn wayland_socket_is_reachable(path: &PathBuf) -> bool {
    UnixStream::connect(path).is_ok()
}

#[cfg(not(unix))]
fn wayland_socket_is_reachable(_: &PathBuf) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::wayland_socket_path;
    use std::path::Path;

    #[test]
    fn absolute_wayland_socket_is_preserved() {
        assert_eq!(wayland_socket_path("/tmp/wayland-9"), Path::new("/tmp/wayland-9"));
    }
}
