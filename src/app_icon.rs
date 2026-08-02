use std::sync::{Arc, OnceLock};

pub(crate) const APP_ID: &str = "Visionary";

static APP_ICON: OnceLock<Arc<egui::IconData>> = OnceLock::new();

/// Return the shared icon used by every native Visionary viewport.
pub(crate) fn viewport_icon() -> Arc<egui::IconData> {
    Arc::clone(APP_ICON.get_or_init(|| {
        let image = image::load_from_memory_with_format(
            include_bytes!("../assets/icons/visionary.png"),
            image::ImageFormat::Png,
        )
        .expect("the embedded Visionary icon must be a valid PNG")
        .into_rgba8();
        let (width, height) = image.dimensions();

        Arc::new(egui::IconData {
            rgba: image.into_raw(),
            width,
            height,
        })
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_icon_is_square_rgba() {
        let icon = viewport_icon();
        assert_eq!(icon.width, 512);
        assert_eq!(icon.height, 512);
        assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
    }
}
