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

    /// NSView's backing layer can already be flipped. Image contents and
    /// nested layer frames then have opposite vertical origins.
    fn patch_frame(
        patch: &CursorPatch,
        scale: f64,
        height: f64,
        root_flipped: bool,
    ) -> CGRect {
        let top = patch.y as f64 / scale;
        let h = patch.height as f64 / scale;
        let y = if root_flipped { height - top - h } else { top };
        CGRect::new(
            CGPoint::new(patch.x as f64 / scale, y),
            CGSize::new(patch.width as f64 / scale, h),
        )
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
            Self::from_root(root?)
        }

        pub(super) fn from_root(root: Retained<CALayer>) -> Option<Self> {
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

        #[cfg(test)]
        pub(super) fn test_container(&self) -> &CALayer {
            &self.container
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
            if !scale.is_finite()
                || scale <= 0.0
                || patch.width == 0
                || patch.height == 0
                || patch.width.checked_mul(patch.height) != Some(patch.pixels.len())
            {
                return false;
            }
            // Always update geometry, even if image bytes have not changed:
            // a resize changes the bottom-origin y without changing the patch.
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            self.container.setFrame(self.root.bounds());
            self.patch.setFrame(patch_frame(
                &patch,
                scale,
                self.container.bounds().size.height,
                self.root.isGeometryFlipped(),
            ));
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

#[cfg(all(test, target_os = "macos"))]
mod position_tests {
    use objc2_core_foundation::{CFData, CFRetained, CGPoint, CGRect, CGSize};
    use objc2_core_graphics::*;
    use objc2_quartz_core::CALayer;

    fn image(pixels: &[u32], width: usize, height: usize) -> CFRetained<CGImage> {
        let bytes: Vec<_> = pixels
            .iter()
            .flat_map(|pixel| pixel.to_le_bytes())
            .collect();
        let data = CFData::from_bytes(&bytes);
        let provider = CGDataProvider::with_cf_data(Some(&data)).unwrap();
        let space = CGColorSpace::new_device_rgb().unwrap();
        unsafe {
            CGImage::new(
                width,
                height,
                8,
                32,
                width * 4,
                Some(&space),
                CGBitmapInfo(
                    CGImageAlphaInfo::NoneSkipFirst.0 | CGImageByteOrderInfo::Order32Little.0,
                ),
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
            .unwrap()
        }
    }

    fn render(root: &CALayer, width: usize, height: usize, scale: f64) -> Vec<u32> {
        let mut output = vec![0u32; width * height];
        let space = CGColorSpace::new_device_rgb().unwrap();
        // SAFETY: the context borrows the allocated pixel buffer only until
        // it is explicitly dropped below, before we inspect or return pixels.
        let context = unsafe {
            CGBitmapContextCreate(
                output.as_mut_ptr().cast(),
                width,
                height,
                8,
                width * 4,
                Some(&space),
                CGImageAlphaInfo::NoneSkipFirst.0 | CGImageByteOrderInfo::Order32Little.0,
            )
            .unwrap()
        };
        CGContext::scale_ctm(Some(&context), scale, scale);
        root.renderInContext(&context);
        drop(context);
        for pixel in &mut output {
            *pixel &= 0xffffff;
        }
        output
    }

    #[test]
    fn native_cursor_aligns_with_framebuffer_pixels() {
        use crate::render::CursorPatch;
        for flipped in [false, true] {
            for scale in [1.0, 2.0] {
                for (x, y) in [(20, 15), (175, 95)] {
                    let (width, height) = (200, 120);
                    let bounds = CGRect::new(
                        CGPoint::new(0.0, 0.0),
                        CGSize::new(width as f64 / scale, height as f64 / scale),
                    );
                    let root = CALayer::new();
                    root.setBounds(bounds);
                    root.setGeometryFlipped(flipped);
                    let reference = CALayer::new();
                    reference.setFrame(root.bounds());
                    reference.setGeometryFlipped(true);
                    reference.setContentsScale(scale);
                    root.addSublayer(&reference);
                    let mut base = vec![0x0000ff; width * height];
                    let mut data = Vec::new();
                    for dy in 0..10 {
                        for dx in 0..5 {
                            let color = 0x100000 + dy * 0x1000 + dx * 0x100;
                            base[(y + dy) * width + x + dx] = color as u32;
                            data.push(color as u32);
                        }
                    }
                    let base_image = image(&base, width, height);
                    // SAFETY: CALayer retains a valid, owned CGImage.
                    unsafe {
                        reference.setContents(Some(base_image.as_ref()));
                    }
                    let expected = render(&root, width, height, scale);
                    let mut cursor = super::platform::CursorLayer::from_root(root.clone()).unwrap();
                    let patch = CursorPatch {
                        x,
                        y,
                        width: 5,
                        height: 10,
                        pixels: data,
                    };
                    assert!(cursor.update(patch.clone(), scale, 700));
                    assert!(render(&root, width, height, scale) == expected,
                        "cursor patch differs from terminal pixels: flipped={flipped}, scale={scale}, x={x}, y={y}");
                    // Resize with identical patch bytes: this takes the cached
                    // image path and must still recompute bottom-origin y.
                    let larger_height = height + 60;
                    root.setBounds(CGRect::new(
                        CGPoint::new(0.0, 0.0),
                        CGSize::new(width as f64 / scale, larger_height as f64 / scale),
                    ));
                    reference.setFrame(root.bounds());
                    let mut larger = vec![0x0000ff; width * larger_height];
                    larger[..base.len()].copy_from_slice(&base);
                    let larger_image = image(&larger, width, larger_height);
                    // SAFETY: valid owned CGImage retained by the layer.
                    unsafe {
                        reference.setContents(Some(larger_image.as_ref()));
                    }
                    cursor.test_container().setHidden(true);
                    let expected = render(&root, width, larger_height, scale);
                    cursor.test_container().setHidden(false);
                    assert!(cursor.update(patch, scale, 700));
                    assert!(
                        render(&root, width, larger_height, scale) == expected,
                        "cached cursor moved after resize: flipped={flipped}, scale={scale}"
                    );
                }
            }
        }
    }
}
