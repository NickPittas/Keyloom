//! The system tray icon: Show Keyloom / Quit. Linux speaks StatusNotifierItem over D-Bus (`ksni`, no GTK in a winit
//! app); Windows uses `tray-icon`. The icon only sends commands; the app decides what they mean (see `App::tray_tick`).
//! If no tray can be created, `start` fails and the app keeps its normal Close = Quit behaviour.
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrayCmd {
    Show,
    Quit,
    /// The desktop's tray went away (panel restarted/closed): the window must come back, never stay hidden.
    Lost,
}

pub struct Tray {
    rx: Receiver<TrayCmd>,
    _hold: Box<dyn std::any::Any>,
}

impl Tray {
    /// A tray with no desktop behind it, fed by the returned sender (headless tests).
    pub fn channel() -> (Tray, Sender<TrayCmd>) {
        let (tx, rx) = channel();
        (Tray { rx, _hold: Box::new(()) }, tx)
    }

    pub fn poll(&self) -> Option<TrayCmd> {
        self.rx.try_recv().ok()
    }

    /// Create the platform tray icon. `wake` repaints/wakes the app when a menu entry is chosen (a hidden window gets no input).
    pub fn start(wake: Arc<dyn Fn() + Send + Sync>) -> Result<Tray, String> {
        let (tx, rx) = channel();
        let hold = platform(tx, wake)?;
        Ok(Tray { rx, _hold: hold })
    }
}

/// A 32x32 filled disc (RGBA) so no image asset has to ship; ponytail: swap for the real logo when one exists.
fn disc_rgba() -> Vec<u8> {
    let mut v = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32 {
        for x in 0..32 {
            let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
            let a = if dx * dx + dy * dy <= 15.0 * 15.0 { 255 } else { 0 };
            v.extend_from_slice(&[0x2d, 0x9c, 0xdb, a]);
        }
    }
    v
}

#[cfg(target_os = "linux")]
fn platform(tx: Sender<TrayCmd>, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn std::any::Any>, String> {
    use ksni::blocking::TrayMethods;
    struct Sni(Sender<TrayCmd>, Arc<dyn Fn() + Send + Sync>);
    impl Sni {
        fn send(&self, c: TrayCmd) {
            let _ = self.0.send(c);
            (self.1)();
        }
    }
    impl ksni::Tray for Sni {
        fn id(&self) -> String {
            "keyloom".into()
        }
        fn title(&self) -> String {
            "Keyloom".into()
        }
        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            // ARGB32, network byte order.
            let data = disc_rgba().chunks(4).flat_map(|p| [p[3], p[0], p[1], p[2]]).collect();
            vec![ksni::Icon { width: 32, height: 32, data }]
        }
        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayCmd::Show);
        }
        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::StandardItem;
            vec![
                StandardItem { label: "Show Keyloom".into(), activate: Box::new(|t: &mut Self| t.send(TrayCmd::Show)), ..Default::default() }.into(),
                StandardItem { label: "Quit".into(), activate: Box::new(|t: &mut Self| t.send(TrayCmd::Quit)), ..Default::default() }.into(),
            ]
        }
        fn watcher_offline(&self, _why: ksni::OfflineReason) -> bool {
            self.send(TrayCmd::Lost);
            true
        }
    }
    // Fails without a session bus or a tray host (StatusNotifierWatcher): the caller then keeps Close = Quit.
    let handle = Sni(tx, wake).spawn().map_err(|e| e.to_string())?;
    Ok(Box::new(handle))
}

#[cfg(windows)]
fn platform(tx: Sender<TrayCmd>, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn std::any::Any>, String> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
    let (show, quit) = (MenuItem::new("Show Keyloom", true, None), MenuItem::new("Quit", true, None));
    let menu = Menu::new();
    menu.append_items(&[&show, &quit]).map_err(|e| e.to_string())?;
    let (show_id, quit_id) = (show.id().clone(), quit.id().clone());
    let (tx2, wake2) = (tx.clone(), wake.clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let c = if e.id == show_id { TrayCmd::Show } else if e.id == quit_id { TrayCmd::Quit } else { return };
        let _ = tx2.send(c);
        wake2();
    }));
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if matches!(e, TrayIconEvent::DoubleClick { .. }) {
            let _ = tx.send(TrayCmd::Show);
            wake();
        }
    }));
    let icon = Icon::from_rgba(disc_rgba(), 32, 32).map_err(|e| e.to_string())?;
    let icon = TrayIconBuilder::new().with_menu(Box::new(menu)).with_tooltip("Keyloom").with_icon(icon).build().map_err(|e| e.to_string())?;
    Ok(Box::new((icon, show, quit)))
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform(_: Sender<TrayCmd>, _: Arc<dyn Fn() + Send + Sync>) -> Result<Box<dyn std::any::Any>, String> {
    Err("no tray on this platform".into())
}
