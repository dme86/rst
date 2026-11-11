//! Install the bundled rst logo as the active macOS application icon.
//!
//! CFBundleIconFile in rst.app covers Launch Services / Finder. AppKit's
//! applicationIconImage additionally controls the Dock and Cmd+Tab while
//! the app is running, including bare cargo-run binaries with no bundle.

use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSImage};
use objc2_foundation::NSData;

const LOGO: &[u8] = include_bytes!("../assets/logo.png");

pub fn install() {
    // Called from winit's ApplicationHandler::resumed, which runs on the
    // AppKit main thread after NSApplication has been initialized.
    let Some(main_thread) = MainThreadMarker::new() else {
        eprintln!("rst: skipping app icon: not on macOS main thread");
        return;
    };
    let data = NSData::with_bytes(LOGO);
    let Some(icon) = NSImage::initWithData(NSImage::alloc(), &data) else {
        eprintln!("rst: unable to decode bundled assets/logo.png");
        return;
    };
    let app = NSApplication::sharedApplication(main_thread);
    // SAFETY: icon is an initialized, non-nil NSImage. AppKit retains
    // the property; objc2 manages the temporary Retained<NSImage>.
    unsafe { app.setApplicationIconImage(Some(&icon)); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_png_matches_readme_asset() {
        assert!(LOGO.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert!(LOGO.len() > 1024);
    }
}
