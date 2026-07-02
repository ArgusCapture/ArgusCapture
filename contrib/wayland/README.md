# Wayland desktop integration

This directory contains the files GNOME/Wayland expects for showing the
Argus Capture application icon in the shell:

- `applications/org.arguscapture.ArgusCapture.desktop`
- `icons/hicolor/.../org.arguscapture.ArgusCapture.{png,svg}`

The desktop file name, icon name, and GTK application ID all match:

- `org.arguscapture.ArgusCapture`

## Install for the current user

From the repository root:

```sh
mkdir -p ~/.local/share
cp -r contrib/wayland/applications ~/.local/share/
cp -r contrib/wayland/icons ~/.local/share/
update-desktop-database ~/.local/share/applications 2>/dev/null || true
gtk-update-icon-cache -f -t ~/.local/share/icons/hicolor 2>/dev/null || true
```

Then restart the shell session, or log out and back in, if GNOME does not pick
up the icon immediately.

## Install system-wide

```sh
sudo cp -r contrib/wayland/applications /usr/local/share/
sudo cp -r contrib/wayland/icons /usr/local/share/
sudo update-desktop-database /usr/local/share/applications 2>/dev/null || true
sudo gtk-update-icon-cache -f -t /usr/local/share/icons/hicolor 2>/dev/null || true
```

## Notes

- The binary still carries its own embedded logo for in-app display.
- GNOME Shell does not use that embedded image for the app icon.
- GNOME Shell uses the desktop entry plus installed icon theme files instead.
- If `argus-capture` is not on `PATH`, adjust `Exec=` and `TryExec=` in
  `applications/org.arguscapture.ArgusCapture.desktop`.
