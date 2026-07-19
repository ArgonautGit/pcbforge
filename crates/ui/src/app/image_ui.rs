use super::*;

impl ConsoleApp {
    pub(super) fn show_image(
        &mut self,
        ui: &mut egui::Ui,
        key: &'static str,
        tex: &egui::TextureHandle,
    ) -> (crate::imgview::ImageXform, egui::Response) {
        let mut view = self.views.images.get(key).copied().unwrap_or_default();
        let out = crate::imgview::show(ui, tex, &mut view);
        self.views.images.insert(key, view);
        out
    }
}
