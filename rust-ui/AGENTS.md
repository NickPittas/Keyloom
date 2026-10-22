# Test isolation — explicit user requirement

Never run GUI tests, native file pickers, click sweeps, or desktop automation on the user's live desktop. Do not use `/tmp/shot.sh`, `hyprctl`, `ydotool`, or `xdg-open` for testing.

Run **all tests**, including headless egui tests, through:

```sh
bash tools/isolated-test.sh /usr/bin/cargo test --offline
```

For rendered GUI tests use:

```sh
bash tools/virtual-display-test.sh /usr/bin/timeout 30 target/release/corridorkey-ui --screenshot target/test.png --screenshot-after 2
```

These wrappers isolate display sockets, D-Bus, network, input devices and temporary files using bubblewrap. The virtual wrapper starts its own Xvfb and uses software Vulkan. Never bypass a failed wrapper or reconnect it to the host display/session bus. Mock native file pickers for automated interaction tests; never search for buttons by sweeping coordinates.

Builds and source inspection can run normally. Tests write only temporary fixtures and build artifacts, never existing project data or real user preferences. Virtual-display tests do not establish live Hyprland drag-and-drop compatibility or hardware playback performance.
