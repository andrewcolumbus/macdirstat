//! MacDirStat's dark appearance: gray-28 root fill, gray-30 panels, Lucida
//! Grande at 13 pt, blue selection, and the blue-pill tab header.
//!
//! Chrome colors come from [`palette()`]. Treemap pixels are content, not
//! chrome, and use the fixed extension palette in `treemap.rs`.

use egui::Color32;

pub struct TextPalette {
    pub primary: Color32,
    pub secondary: Color32,
    pub label: Color32,
    pub icon: Color32,
    pub muted: Color32,
}

pub struct SurfacePalette {
    pub root: Color32,
    pub panel: Color32,
    pub card: Color32,
    pub divider: Color32,
    pub tab_bar: Color32,
    pub tab_active: Color32,
    pub field: Color32,
    pub card_stroke: Color32,
    pub row_alt: Color32,
}

pub struct WidgetPalette {
    pub noninteractive: Color32,
    pub noninteractive_stroke: Color32,
    pub inactive: Color32,
    pub inactive_stroke: Color32,
    pub hovered: Color32,
    pub hovered_stroke: Color32,
    pub active: Color32,
    pub active_stroke: Color32,
    pub open: Color32,
    pub open_stroke: Color32,
}

pub struct AccentPalette {
    pub cornflower: Color32,
    pub selection_fill: Color32,
    pub readout: Color32,
}

pub struct StatusPalette {
    pub ok: Color32,
    pub warning: Color32,
    pub error: Color32,
}

pub struct ThemePalette {
    pub text: TextPalette,
    pub surface: SurfacePalette,
    pub widgets: WidgetPalette,
    pub accent: AccentPalette,
    pub status: StatusPalette,
}

const DARK: ThemePalette = ThemePalette {
    text: TextPalette {
        primary: Color32::from_gray(210),
        secondary: Color32::from_gray(200),
        label: Color32::from_gray(180),
        icon: Color32::from_gray(160),
        muted: Color32::from_gray(120),
    },
    surface: SurfacePalette {
        root: Color32::from_gray(28),
        panel: Color32::from_gray(30),
        card: Color32::from_gray(40),
        divider: Color32::from_gray(18),
        tab_bar: Color32::from_gray(28),
        tab_active: Color32::from_rgb(65, 125, 195),
        field: Color32::from_gray(22),
        card_stroke: Color32::from_gray(50),
        row_alt: Color32::from_gray(34),
    },
    widgets: WidgetPalette {
        noninteractive: Color32::from_gray(38),
        noninteractive_stroke: Color32::from_gray(55),
        inactive: Color32::from_gray(50),
        inactive_stroke: Color32::from_gray(65),
        hovered: Color32::from_gray(65),
        hovered_stroke: Color32::from_gray(90),
        active: Color32::from_gray(75),
        active_stroke: Color32::from_gray(110),
        open: Color32::from_gray(48),
        open_stroke: Color32::from_gray(75),
    },
    accent: AccentPalette {
        cornflower: Color32::from_rgb(100, 149, 237),
        selection_fill: Color32::from_rgb(56, 117, 215),
        // The top bar's green numeric readout (the "60.0 fps" style).
        readout: Color32::from_rgb(100, 255, 100),
    },
    status: StatusPalette {
        ok: Color32::from_rgb(80, 200, 80),
        warning: Color32::from_rgb(200, 200, 80),
        error: Color32::from_rgb(200, 80, 80),
    },
};

pub fn palette() -> &'static ThemePalette {
    &DARK
}

/// Shared spacing constants.
pub mod spacing {
    pub const GROUP: f32 = 8.0;
    pub const ROW: f32 = 4.0;
    pub const TIGHT: f32 = 2.0;
    pub const SCROLL_BAR_MARGIN: f32 = 14.0;
    pub const TAB_BAR_HEIGHT: f32 = 24.0;
    pub const CORNER_RADIUS: f32 = 4.0;
}

/// Install the font stack: Lucida Grande first, egui's
/// defaults next, then Noto Sans for Unicode coverage. Lucida Grande ships
/// with macOS and is loaded from the system rather than redistributed; if it
/// is missing the stack falls back to egui's default font.
pub fn install_fonts(ctx: &egui::Context) {
    use std::sync::Arc;

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "NotoSans".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/fonts/NotoSans-Regular.ttf"
        ))),
    );
    let mut bold_stack = vec!["NotoSans".to_owned()];
    if let Ok(bytes) = std::fs::read(SYSTEM_LUCIDA) {
        let mut regular = egui::FontData::from_owned(bytes.clone());
        regular.index = 0;
        let mut bold = egui::FontData::from_owned(bytes);
        bold.index = 1;
        fonts
            .font_data
            .insert("LucidaGrande".to_owned(), Arc::new(regular));
        fonts
            .font_data
            .insert("LucidaGrandeBold".to_owned(), Arc::new(bold));
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .expect("egui always defines a proportional family")
            .insert(0, "LucidaGrande".to_owned());
        bold_stack.insert(0, "LucidaGrandeBold".to_owned());
    }
    fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .expect("egui always defines a proportional family")
        .push("NotoSans".to_owned());
    fonts
        .families
        .insert(egui::FontFamily::Name(BOLD_FAMILY.into()), bold_stack);
    fonts
        .families
        .get_mut(&egui::FontFamily::Monospace)
        .expect("egui always defines a monospace family")
        .push("NotoSans".to_owned());
    ctx.set_fonts(fonts);
}

const SYSTEM_LUCIDA: &str = "/System/Library/Fonts/LucidaGrande.ttc";

const BOLD_FAMILY: &str = "Lucida Grande Bold";

pub fn bold_font(size: f32) -> egui::FontId {
    egui::FontId::new(size, egui::FontFamily::Name(BOLD_FAMILY.into()))
}

/// Install the style into both of egui's theme slots so no popup or
/// secondary context falls back to egui's defaults.
pub fn configure_context(ctx: &egui::Context) {
    let style = build_style();
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    ctx.set_theme(egui::Theme::Dark);
}

/// The dark style.
pub fn build_style() -> egui::Style {
    use egui::{FontId, TextStyle};

    let p = palette();
    let mut style = egui::Theme::Dark.default_style();

    style.visuals.dark_mode = true;
    style.visuals.override_text_color = None;
    style.visuals.panel_fill = p.surface.root;
    style.visuals.window_fill = Color32::from_gray(32);
    style.visuals.extreme_bg_color = Color32::from_gray(18);
    style.visuals.faint_bg_color = p.surface.row_alt;
    style.visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 2],
        blur: 8,
        spread: 0,
        color: Color32::from_black_alpha(60),
    };

    let set_widget = |visuals: &mut egui::style::WidgetVisuals, fill: Color32, stroke: Color32| {
        visuals.bg_fill = fill;
        visuals.weak_bg_fill = fill;
        visuals.bg_stroke = egui::Stroke::new(1.0, stroke);
    };
    let w = &p.widgets;
    set_widget(
        &mut style.visuals.widgets.noninteractive,
        w.noninteractive,
        w.noninteractive_stroke,
    );
    set_widget(
        &mut style.visuals.widgets.inactive,
        w.inactive,
        w.inactive_stroke,
    );
    set_widget(
        &mut style.visuals.widgets.hovered,
        w.hovered,
        w.hovered_stroke,
    );
    set_widget(&mut style.visuals.widgets.active, w.active, w.active_stroke);
    set_widget(&mut style.visuals.widgets.open, w.open, w.open_stroke);

    style.visuals.selection.bg_fill = p.accent.selection_fill;
    style.visuals.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
    style.visuals.window_corner_radius = egui::CornerRadius::same(5);
    style.visuals.menu_corner_radius = egui::CornerRadius::same(5);
    style.visuals.text_options.subpixel_binning = false;

    style.spacing.button_padding = egui::vec2(10.0, 4.0);
    style.spacing.item_spacing = egui::vec2(8.0, 4.0);
    style.spacing.window_margin = egui::Margin::same(8);
    style.spacing.interact_size = egui::vec2(40.0, 20.0);
    style.spacing.scroll.dormant_handle_opacity = 0.4;
    style.spacing.scroll.dormant_background_opacity = 0.0;
    style.spacing.scroll.floating_allocated_width = spacing::SCROLL_BAR_MARGIN;

    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(13.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(11.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(11.0));
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Monospace, FontId::monospace(12.0));

    style.interaction.tooltip_delay = 0.5;
    style.interaction.selectable_labels = false;
    style
}

/// A panel body: gray-30 fill with no inner margin, like a tiled cell.
pub fn panel_frame() -> egui::Frame {
    egui::Frame::new().fill(palette().surface.panel)
}

/// Paint a panel header: a 24 px gray-28 strip with
/// the hamburger glyph and one active blue tab with a 2 px accent underline.
/// `extra` renders right-aligned header content.
pub fn panel_header(
    ui: &mut egui::Ui,
    title: &str,
    extra: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let p = palette();
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(
        egui::vec2(width, spacing::TAB_BAR_HEIGHT),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(strip, 0.0, p.surface.tab_bar);

    let mut header_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(strip.shrink2(egui::vec2(4.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    header_ui.add(egui::Label::new(
        egui::RichText::new("\u{2630}").color(p.text.icon),
    ));
    header_ui.add_space(spacing::TIGHT);
    let tab = header_ui
        .add(egui::Button::new(egui::RichText::new(title).color(Color32::WHITE)).selected(true));
    let r = tab.rect;
    header_ui.painter().rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(r.left(), r.bottom() - 2.0),
            egui::vec2(r.width(), 2.0),
        ),
        0.0,
        p.surface.tab_active,
    );
    header_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), extra);
    tab
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative_luminance(color: Color32) -> f32 {
        fn channel(value: u8) -> f32 {
            let c = f32::from(value) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn body_text_is_readable_on_panels() {
        let p = palette();
        for surface in [
            p.surface.panel,
            p.surface.root,
            p.surface.card,
            p.surface.row_alt,
        ] {
            assert!(contrast(p.text.primary, surface) >= 7.0);
            assert!(contrast(p.text.label, surface) >= 4.5);
        }
    }

    #[test]
    fn style_matches_dark_theme_metrics() {
        let style = build_style();
        assert_eq!(style.visuals.panel_fill, Color32::from_gray(28));
        assert_eq!(style.spacing.interact_size, egui::vec2(40.0, 20.0));
        assert_eq!(style.spacing.button_padding, egui::vec2(10.0, 4.0));
        assert_eq!(
            style.text_styles[&egui::TextStyle::Body],
            egui::FontId::proportional(13.0)
        );
        assert_eq!(
            style.visuals.selection.bg_fill,
            Color32::from_rgb(56, 117, 215)
        );
    }
}
