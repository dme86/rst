//! macOS compositor cursor: tiny image patch, no timer-driven full-window presents.
#[cfg(target_os = "macos")]
mod platform {
    use crate::render::CursorPatch;
    use objc2::{msg_send, rc::Retained, runtime::AnyObject, MainThreadMarker};
    use objc2_core_foundation::{CFData, CFRetained, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage,
        CGImageAlphaInfo, CGImageByteOrderInfo,
    };
    use objc2_foundation::{ns_string, NSArray, NSNumber, NSObject};
    use objc2_quartz_core::{
        kCAAnimationDiscrete, CAKeyframeAnimation, CALayer, CAMediaTiming, CATransaction,
    };
    use winit::{
        raw_window_handle::{HasWindowHandle, RawWindowHandle},
        window::Window,
    };

    pub struct CursorLayer {
        container: Retained<CALayer>,
        patch: Retained<CALayer>,
        root: Retained<CALayer>,
        color_space: CFRetained<CGColorSpace>,
        current: Option<(CursorPatch, u64, u64)>,
    }

    impl CursorLayer {
        pub fn new(window: &Window) -> Option<Self> {
            MainThreadMarker::new()?;
            let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
                return None;
            };
            // SAFETY: winit owns this NSView, and we are on the AppKit main thread.
            let view = unsafe { handle.ns_view.cast::<NSObject>().as_ref() };
            let root: Option<Retained<CALayer>> = unsafe { msg_send![view, layer] };
            let root = root?;
            let color_space = CGColorSpace::new_device_rgb()?;
            let container = CALayer::new();
            let patch = CALayer::new();
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            container.setFrame(root.bounds());
            container.setGeometryFlipped(true);
            patch.setGeometryFlipped(true);
            patch.setHidden(true);
            container.addSublayer(&patch);
            root.addSublayer(&container);
            CATransaction::commit();
            Some(Self {
                container,
                patch,
                root,
                color_space,
                current: None,
            })
        }

        /// Profiling observes compositor state only on existing events; it
        /// never adds a sampling timer or wakes the event loop for blinking.
        pub fn phase(&self) -> Option<f32> {
            self.current.as_ref()?;
            // SAFETY: this is a CALayer instance, not a custom subclass;
            // its presentation copy therefore has the same CALayer type.
            unsafe { self.patch.presentationLayer() }.map(|layer| layer.opacity())
        }

        pub fn hide(&mut self) {
            if self.current.take().is_some() {
                CATransaction::begin();
                CATransaction::setDisableActions(true);
                self.patch.setHidden(true);
                self.patch
                    .removeAnimationForKey(ns_string!("rst-cursor-blink"));
                // SAFETY: nil releases the previous, valid CGImage contents.
                unsafe {
                    self.patch.setContents(None);
                }
                CATransaction::commit();
            }
        }

        pub fn update(&mut self, patch: CursorPatch, scale: f64, interval_ms: u64) -> bool {
            // The window can resize without changing the cursor's pixels.
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            self.container.setFrame(self.root.bounds());
            CATransaction::commit();
            if self
                .current
                .as_ref()
                .is_some_and(|(old, old_scale, interval)| {
                    old == &patch && *old_scale == scale.to_bits() && *interval == interval_ms
                })
            {
                return true;
            }
            if !scale.is_finite()
                || scale <= 0.0
                || patch.width == 0
                || patch.height == 0
                || patch.width.checked_mul(patch.height) != Some(patch.pixels.len())
            {
                return false;
            }
            // CFData copies the small patch and owns its bytes; no borrowed
            // framebuffer pointers or release callbacks cross the native boundary.
            let bytes: Vec<_> = patch
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_le_bytes())
                .collect();
            let data = CFData::from_bytes(&bytes);
            let Some(provider) = CGDataProvider::with_cf_data(Some(&data)) else {
                return false;
            };
            let info = CGBitmapInfo(
                CGImageAlphaInfo::NoneSkipFirst.0 | CGImageByteOrderInfo::Order32Little.0,
            );
            // SAFETY: 32-bit little-endian pixels, exact checked dimensions,
            // owned provider data, and no custom decode table.
            let Some(image) = (unsafe {
                CGImage::new(
                    patch.width,
                    patch.height,
                    8,
                    32,
                    patch.width * 4,
                    Some(&self.color_space),
                    info,
                    Some(&provider),
                    std::ptr::null(),
                    false,
                    CGColorRenderingIntent::RenderingIntentDefault,
                )
            }) else {
                return false;
            };
            let animation = CAKeyframeAnimation::animationWithKeyPath(Some(ns_string!("opacity")));
            let one = NSNumber::new_f64(1.0);
            let zero = NSNumber::new_f64(0.0);
            let values: Retained<NSArray<AnyObject>> = NSArray::from_slice(&[&one, &zero, &zero]);
            // SAFETY: opacity animations require NSNumber values.
            unsafe {
                animation.setValues(Some(&values));
                animation.setCalculationMode(kCAAnimationDiscrete);
            }
            // Key times must be increasing: on for one interval, then off.
            animation.setKeyTimes(Some(&NSArray::<NSNumber>::from_slice(&[
                &zero,
                &NSNumber::new_f64(0.5),
                &one,
            ])));
            animation.setDuration(interval_ms.max(1) as f64 / 500.0);
            animation.setRepeatCount(f32::INFINITY);
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            self.container.setFrame(self.root.bounds());
            self.patch.setFrame(CGRect::new(
                CGPoint::new(patch.x as f64 / scale, patch.y as f64 / scale),
                CGSize::new(patch.width as f64 / scale, patch.height as f64 / scale),
            ));
            self.patch.setContentsScale(scale);
            self.patch.setOpacity(1.0);
            // SAFETY: CALayer accepts CGImage contents and retains the image.
            unsafe {
                self.patch.setContents(Some(image.as_ref()));
            }
            self.patch.setHidden(false);
            self.patch
                .addAnimation_forKey(&animation, Some(ns_string!("rst-cursor-blink")));
            CATransaction::commit();
            self.current = Some((patch, scale.to_bits(), interval_ms));
            true
        }
    }

    impl Drop for CursorLayer {
        fn drop(&mut self) {
            self.container.removeFromSuperlayer();
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    pub struct CursorLayer;
    impl CursorLayer {
        pub fn new(_: &winit::window::Window) -> Option<Self> {
            None
        }
        pub fn phase(&self) -> Option<f32> {
            None
        }
        pub fn hide(&mut self) {}
        pub fn update(&mut self, _: crate::render::CursorPatch, _: f64, _: u64) -> bool {
            false
        }
    }
}

pub use platform::CursorLayer;
