//! The Mac menu bar icon with a colored dot. A template image takes the bar's color but
//! loses the dot's; a plain image keeps the dot but stays black on a dark bar. So the image
//! draws itself each time the bar draws it: the head in the color of the bar's text, as
//! AppKit resolves it at that moment (light or dark bar, over any wallpaper), then the dot.

use objc2::{AnyThread, MainThreadMarker};
use objc2::rc::Retained;
use objc2::runtime::Bool;
use objc2_app_kit::{NSColor, NSCompositingOperation, NSImage, NSRectFillUsingOperation, NSStatusItem};
use objc2_foundation::{NSData, NSRect, NSSize};

/// The icon's size in the bar, in points.
const POINTS: f64 = 18.0;

fn image(png: &[u8]) -> Option<Retained<NSImage>> {
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png))?;
    image.setSize(NSSize::new(POINTS, POINTS));
    Some(image)
}

/// Puts the two layers (icon.rs, `macos_layers`) on the status item's button. Main thread.
pub fn show(item: &NSStatusItem, head: &[u8], dot: &[u8]) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let (Some(head), Some(dot)) = (image(head), image(dot)) else { return };
    let draw = block2::RcBlock::new(move |rect: NSRect| -> Bool {
        head.drawInRect(rect);
        // Only where the head is: the color of the text, over the black.
        NSColor::labelColor().set();
        NSRectFillUsingOperation(rect, NSCompositingOperation::SourceAtop);
        dot.drawInRect(rect);
        Bool::YES
    });
    let composed = NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(POINTS, POINTS), false, &draw);
    composed.setTemplate(false);
    if let Some(button) = item.button(mtm) {
        button.setImage(Some(&composed));
    }
}
