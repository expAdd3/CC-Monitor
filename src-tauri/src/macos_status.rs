use block2::RcBlock;
use objc2::{
    rc::Retained,
    runtime::{AnyObject, Bool},
    AnyThread, MainThreadMarker,
};
use objc2_app_kit::{
    NSAttributedStringNSStringDrawing, NSBezierPath, NSCellImagePosition, NSColor,
    NSCompositingOperation, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSImage,
    NSImageSymbolConfiguration, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSRectFillUsingOperation, NSTextAlignment, NSTextTab,
};
use objc2_foundation::{
    NSArray, NSData, NSDictionary, NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize,
    NSString,
};
use tauri::{tray::TrayIcon, Runtime};

pub fn update<R: Runtime>(tray: &TrayIcon<R>, running: i64, waiting: i64, needs_input: i64) {
    let values = [
        running.max(0).to_string(),
        waiting.max(0).to_string(),
        needs_input.max(0).to_string(),
    ];
    let _ = tray.with_inner_tray_icon(move |inner| {
        let Some(status_item) = inner.ns_status_item() else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(button) = status_item.button(mtm) else {
            return;
        };

        let Some(icon) = load_menu_bar_icon() else {
            return;
        };
        let image = render_status_image(icon, values);
        image.setTemplate(false);
        button.setImage(Some(&image));
        button.setImagePosition(NSCellImagePosition::ImageOnly);
        button.setTitle(&NSString::from_str(""));
    });
}

pub struct SessionMenuRow {
    pub primary: String,
    pub detail: String,
    pub state: String,
}

pub fn style_menu_rows<R: Runtime>(tray: &TrayIcon<R>, session_rows: Vec<SessionMenuRow>) {
    let _ = tray.with_inner_tray_icon(move |inner| {
        let Some(status_item) = inner.ns_status_item() else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(menu) = status_item.menu(mtm) else {
            return;
        };
        for item in menu.itemArray().iter() {
            let title = item.title().to_string();
            if let Some(row) = session_rows.iter().find(|row| row.primary == title) {
                let font = NSFont::systemFontOfSize_weight(12.0, 0.0);
                item.setAttributedTitle(Some(&attributed_columns(
                    &title,
                    &font,
                    &NSColor::labelColor(),
                )));
                item.setImage(status_symbol(&row.state).as_deref());
                continue;
            }
            if session_rows.iter().any(|row| row.detail == title) {
                let font = NSFont::systemFontOfSize_weight(11.0, 0.0);
                item.setAttributedTitle(Some(&attributed_columns(
                    &title,
                    &font,
                    &NSColor::secondaryLabelColor(),
                )));
                continue;
            }
            let (font, color) = if title == "CC Monitor" {
                (
                    NSFont::systemFontOfSize_weight(12.0, 0.35),
                    NSColor::labelColor(),
                )
            } else if title.starts_with("最近活跃会话（") {
                (
                    NSFont::systemFontOfSize_weight(10.0, 0.35),
                    NSColor::secondaryLabelColor(),
                )
            } else if title.contains("需要介入") || title.contains("失败") {
                (
                    NSFont::systemFontOfSize_weight(11.0, 0.35),
                    NSColor::systemRedColor(),
                )
            } else if title.starts_with("今日 ")
                || title == "暂无活跃会话"
                || title == "当前没有活跃会话"
                || title.contains(" 个会话")
            {
                (
                    NSFont::systemFontOfSize_weight(11.0, 0.0),
                    NSColor::secondaryLabelColor(),
                )
            } else {
                continue;
            };
            item.setAttributedTitle(Some(&attributed(&title, &font, &color)));
        }
    });
}

fn status_symbol(state: &str) -> Option<Retained<NSImage>> {
    let (symbol, description, color) = match state {
        "running" => ("play.circle.fill", "运行中", NSColor::systemGreenColor()),
        "waiting" => ("pause.circle.fill", "等待中", NSColor::systemYellowColor()),
        "needs_input" => (
            "exclamationmark.circle.fill",
            "需要介入",
            NSColor::systemOrangeColor(),
        ),
        "failed" => ("xmark.circle.fill", "失败", NSColor::systemRedColor()),
        _ => (
            "questionmark.circle",
            "状态未知",
            NSColor::secondaryLabelColor(),
        ),
    };
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(symbol),
        Some(&NSString::from_str(description)),
    )?;
    let configuration = NSImageSymbolConfiguration::configurationWithHierarchicalColor(&color);
    let image = image.imageWithSymbolConfiguration(&configuration)?;
    image.setSize(NSSize::new(13.0, 13.0));
    image.setTemplate(false);
    Some(image)
}

fn load_menu_bar_icon() -> Option<Retained<NSImage>> {
    let data = NSData::with_bytes(include_bytes!("../../assets/menubar_color@2x.png"));
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    image.setSize(NSSize::new(22.0, 22.0));
    Some(image)
}

fn render_status_image(icon: Retained<NSImage>, values: [String; 3]) -> Retained<NSImage> {
    const HEIGHT: f64 = 22.0;
    const ICON_WIDTH: f64 = 22.0;
    const GAP: f64 = 5.0;
    const DOT_RADIUS: f64 = 2.0;
    const DOT_NUMBER_GAP: f64 = 2.0;

    let font = NSFont::monospacedDigitSystemFontOfSize_weight(7.0, 0.0);
    let label_color = NSColor::textColor();
    let labels = values.map(|value| attributed(&value, &font, &label_color));
    let number_width = labels
        .iter()
        .map(|label| label.size().width)
        .fold(0.0_f64, f64::max);
    let dot_x = ICON_WIDTH + GAP;
    let number_x = dot_x + DOT_RADIUS * 2.0 + DOT_NUMBER_GAP;
    let total_width = number_x + number_width + 3.0;

    let drawing = RcBlock::new(move |_destination: NSRect| {
        let text_color = NSColor::textColor();
        let icon_rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(ICON_WIDTH, ICON_WIDTH));
        icon.drawInRect_fromRect_operation_fraction(
            icon_rect,
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(0.0, 0.0)),
            NSCompositingOperation::SourceOver,
            1.0,
        );
        text_color.set();
        NSRectFillUsingOperation(icon_rect, NSCompositingOperation::SourceIn);

        let rows = [
            (17.0, NSColor::systemGreenColor()),
            (10.0, NSColor::systemYellowColor()),
            (3.0, NSColor::systemRedColor()),
        ];
        for (index, (center_y, color)) in rows.into_iter().enumerate() {
            color.set();
            NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                NSPoint::new(dot_x, center_y - DOT_RADIUS),
                NSSize::new(DOT_RADIUS * 2.0, DOT_RADIUS * 2.0),
            ))
            .fill();
            let size = labels[index].size();
            labels[index].drawAtPoint(NSPoint::new(number_x, center_y - size.height / 2.0));
        }
        Bool::YES
    });

    NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(total_width, HEIGHT), false, &drawing)
}

fn attributed(value: &str, font: &NSFont, color: &NSColor) -> Retained<NSMutableAttributedString> {
    let text = NSString::from_str(value);
    let result =
        NSMutableAttributedString::initWithString(NSMutableAttributedString::alloc(), &text);
    let range = NSRange::new(0, value.encode_utf16().count());
    unsafe {
        result.addAttribute_value_range(NSFontAttributeName, font, range);
        result.addAttribute_value_range(NSForegroundColorAttributeName, color, range);
    }
    result
}

fn attributed_columns(
    value: &str,
    font: &NSFont,
    color: &NSColor,
) -> Retained<NSMutableAttributedString> {
    const RIGHT_EDGE: f64 = 300.0;
    let result = attributed(value, font, color);
    let options = NSDictionary::<NSString, AnyObject>::new();
    let tab = unsafe {
        NSTextTab::initWithTextAlignment_location_options(
            NSTextTab::alloc(),
            NSTextAlignment(2),
            RIGHT_EDGE,
            &options,
        )
    };
    let tabs = NSArray::from_retained_slice(&[tab]);
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setTabStops(Some(&tabs));
    let range = NSRange::new(0, value.encode_utf16().count());
    unsafe {
        result.addAttribute_value_range(NSParagraphStyleAttributeName, &paragraph, range);
    }
    result
}
